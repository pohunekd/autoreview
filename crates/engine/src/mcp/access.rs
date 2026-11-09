//! The clients of the MCP server: their scopes, token digests and the bearer token check.
//!
//! The configuration holds only the SHA-256 digest of each token. A token is a random secret
//! of at least 32 bytes, so a plain digest cannot be reversed or guessed, and no salt or slow
//! hash is needed.

// Rust guideline compliant 2026-06-26

use std::net::SocketAddr;

use sha2::{Digest, Sha256};

use crate::error::Error;

/// Error code of an `mcp` key whose value the loader refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Length of a token digest in bytes.
const DIGEST_BYTES: usize = 32;

/// Longest client name, so that every log line stays short.
const CLIENT_NAME_MAX: usize = 32;

/// Scheme of the `Authorization` header, compared without case as RFC 9110 says.
const BEARER_SCHEME: &str = "bearer";

/// What a client may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Calls the read tools only.
    Read,
    /// Calls the read tools and the write tools.
    Write,
}

impl Scope {
    /// Parses the scope of the configuration: `read` or `write`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            _ => None,
        }
    }

    /// Reports whether a client of this scope may call a tool that needs `needed`.
    #[must_use]
    pub fn allows(self, needed: Self) -> bool {
        self == Self::Write || needed == Self::Read
    }
}

/// One client of the MCP server, as the configuration names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpClient {
    /// Name of the client in the logs.
    pub name: String,
    /// What the client may do.
    pub scope: Scope,
    /// SHA-256 digest of the client's bearer token.
    pub token_sha256: [u8; DIGEST_BYTES],
}

/// One client entry as the configuration file writes it, before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpClientEntry {
    /// Name of the client in the logs.
    pub name: String,
    /// `read` or `write`.
    pub scope: String,
    /// SHA-256 digest of the token, 64 lowercase hexadecimal characters.
    pub token_sha256: String,
}

/// Settings of the `autoreview mcp` command, the `mcp` section of the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    /// Address the server listens on, behind the TLS reverse proxy.
    pub listen: SocketAddr,
    /// `Host` header values the server accepts, the public names of the proxy.
    pub allowed_hosts: Vec<String>,
    /// The clients, each with its own token.
    pub clients: Vec<McpClient>,
}

impl McpConfig {
    /// Validates the `mcp` section and builds the server settings.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` naming `mcp.listen` for an address that does not
    /// parse, `mcp.allowed_hosts` for an empty list or a blank or spaced host, and
    /// `mcp.clients` for no client, a repeated name or digest, a bad name, scope or digest.
    pub fn new(
        listen: &str,
        allowed_hosts: Vec<String>,
        entries: &[McpClientEntry],
    ) -> Result<Self, Error> {
        let listen = listen
            .parse::<SocketAddr>()
            .map_err(|_| invalid("mcp.listen", "must be an IP address and port"))?;
        if allowed_hosts.is_empty()
            || allowed_hosts
                .iter()
                .any(|host| host.is_empty() || host.chars().any(char::is_whitespace))
        {
            return Err(invalid(
                "mcp.allowed_hosts",
                "must list host names without spaces",
            ));
        }
        if entries.is_empty() {
            return Err(invalid("mcp.clients", "must list at least one client"));
        }
        let mut clients: Vec<McpClient> = Vec::with_capacity(entries.len());
        for entry in entries {
            let client = client(entry)?;
            if clients.iter().any(|known| known.name == client.name) {
                return Err(invalid("mcp.clients", "client names must be unique"));
            }
            if clients
                .iter()
                .any(|known| known.token_sha256 == client.token_sha256)
            {
                return Err(invalid("mcp.clients", "token digests must be unique"));
            }
            clients.push(client);
        }
        Ok(Self {
            listen,
            allowed_hosts,
            clients,
        })
    }
}

/// Why a request was refused before any tool ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No `Authorization` header, or one that is not a bearer token.
    MissingToken,
    /// A bearer token whose digest no client has.
    UnknownToken,
}

impl Refusal {
    /// Returns the reason of the refusal, as the logs write it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MissingToken => "missing_token",
            Self::UnknownToken => "unknown_token",
        }
    }
}

/// Returns the client whose token the `Authorization` header carries.
///
/// Every digest is compared in full, so the time does not show which client matched.
pub(crate) fn authorize<'a>(
    clients: &'a [McpClient],
    authorization: Option<&[u8]>,
) -> Result<&'a McpClient, Refusal> {
    let token = authorization
        .and_then(bearer_token)
        .ok_or(Refusal::MissingToken)?;
    let digest: [u8; DIGEST_BYTES] = Sha256::digest(token).into();
    let mut found = None;
    for client in clients {
        if digests_equal(&client.token_sha256, &digest) {
            found = Some(client);
        }
    }
    found.ok_or(Refusal::UnknownToken)
}

/// Returns the token of a `Bearer TOKEN` header value, or `None` for any other value.
fn bearer_token(value: &[u8]) -> Option<&[u8]> {
    let text = std::str::from_utf8(value).ok()?;
    let (scheme, token) = text.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case(BEARER_SCHEME)
        || token.is_empty()
        || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    Some(token.as_bytes())
}

/// Compares two digests without stopping at the first difference.
fn digests_equal(left: &[u8; DIGEST_BYTES], right: &[u8; DIGEST_BYTES]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

/// Validates one client entry.
fn client(entry: &McpClientEntry) -> Result<McpClient, Error> {
    let name_valid = !entry.name.is_empty()
        && entry.name.len() <= CLIENT_NAME_MAX
        && entry
            .name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !name_valid {
        return Err(invalid(
            "mcp.clients",
            "a name has 1 to 32 lowercase letters, digits or hyphens",
        ));
    }
    let Some(scope) = Scope::parse(&entry.scope) else {
        return Err(invalid("mcp.clients", "a scope is read or write"));
    };
    let lower = entry
        .token_sha256
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let decoded = hex::decode(&entry.token_sha256).ok().filter(|_| lower);
    let Some(token_sha256) = decoded.and_then(|bytes| <[u8; DIGEST_BYTES]>::try_from(bytes).ok())
    else {
        return Err(invalid(
            "mcp.clients",
            "token_sha256 is 64 lowercase hexadecimal characters",
        ));
    };
    Ok(McpClient {
        name: entry.name.clone(),
        scope,
        token_sha256,
    })
}

/// Builds a `configuration_invalid` error that names `key`.
fn invalid(key: &str, message: &'static str) -> Error {
    Error::config(CONFIGURATION_INVALID, key, Some(Box::from(message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the digest of `token` as `printf %s TOKEN | sha256sum` prints it.
    fn digest_hex(token: &str) -> String {
        hex::encode(Sha256::digest(token.as_bytes()))
    }

    fn entry(name: &str, scope: &str, token: &str) -> McpClientEntry {
        McpClientEntry {
            name: name.to_owned(),
            scope: scope.to_owned(),
            token_sha256: digest_hex(token),
        }
    }

    fn config(entries: &[McpClientEntry]) -> Result<McpConfig, Error> {
        McpConfig::new(
            "127.0.0.1:8787",
            vec!["mcp.example.com".to_owned()],
            entries,
        )
    }

    /// A header value and the client name it authorizes, or the refusal.
    type Case<'a> = (Option<&'a [u8]>, Result<&'a str, Refusal>);

    fn refused_key(result: Result<McpConfig, Error>) -> String {
        match result {
            Err(Error::Config { code, name, .. }) if code == CONFIGURATION_INVALID => name,
            other => panic!("expected configuration_invalid, got {other:?}"),
        }
    }

    #[test]
    fn authorize_finds_the_client_of_the_token_and_refuses_the_rest() {
        let config = config(&[
            entry("reader", "read", "alpha-token"),
            entry("writer", "write", "beta-token"),
        ])
        .expect("the clients are valid");
        let cases: [Case<'_>; 9] = [
            (Some(b"Bearer alpha-token"), Ok("reader")),
            (Some(b"bearer beta-token"), Ok("writer")),
            (Some(b"BEARER beta-token"), Ok("writer")),
            (None, Err(Refusal::MissingToken)),
            (Some(b"Bearer "), Err(Refusal::MissingToken)),
            (Some(b"Basic alpha-token"), Err(Refusal::MissingToken)),
            (
                Some(b"Bearer alpha-token extra"),
                Err(Refusal::MissingToken),
            ),
            (Some(b"Bearer\xffalpha"), Err(Refusal::MissingToken)),
            (Some(b"Bearer gamma-token"), Err(Refusal::UnknownToken)),
        ];
        for (header, expected) in cases {
            let found = authorize(&config.clients, header).map(|client| client.name.as_str());
            assert_eq!(found, expected, "header {header:?}");
        }
    }

    #[test]
    fn scope_allows_writes_only_to_write_clients() {
        assert!(Scope::Read.allows(Scope::Read));
        assert!(!Scope::Read.allows(Scope::Write));
        assert!(Scope::Write.allows(Scope::Read));
        assert!(Scope::Write.allows(Scope::Write));
    }

    #[test]
    fn mcp_config_refuses_every_invalid_value_by_key() {
        let valid = entry("reader", "read", "alpha-token");
        let mut upper = valid.clone();
        upper.token_sha256 = upper.token_sha256.to_uppercase();
        let mut short = valid.clone();
        short.token_sha256.truncate(62);
        let cases: [(&str, Vec<String>, Vec<McpClientEntry>, &str); 9] = [
            (
                "localhost:8787",
                vec!["h".to_owned()],
                vec![valid.clone()],
                "mcp.listen",
            ),
            (
                "127.0.0.1:8787",
                Vec::new(),
                vec![valid.clone()],
                "mcp.allowed_hosts",
            ),
            (
                "127.0.0.1:8787",
                vec![String::new()],
                vec![valid.clone()],
                "mcp.allowed_hosts",
            ),
            (
                "127.0.0.1:8787",
                vec!["a b".to_owned()],
                vec![valid.clone()],
                "mcp.allowed_hosts",
            ),
            (
                "127.0.0.1:8787",
                vec!["h".to_owned()],
                Vec::new(),
                "mcp.clients",
            ),
            (
                "127.0.0.1:8787",
                vec!["h".to_owned()],
                vec![entry("Reader", "read", "x")],
                "mcp.clients",
            ),
            (
                "127.0.0.1:8787",
                vec!["h".to_owned()],
                vec![entry("reader", "admin", "x")],
                "mcp.clients",
            ),
            (
                "127.0.0.1:8787",
                vec!["h".to_owned()],
                vec![upper],
                "mcp.clients",
            ),
            (
                "127.0.0.1:8787",
                vec!["h".to_owned()],
                vec![short],
                "mcp.clients",
            ),
        ];
        for (listen, hosts, entries, key) in cases {
            let result = McpConfig::new(listen, hosts, &entries);
            assert_eq!(
                refused_key(result),
                key,
                "listen {listen}, entries {entries:?}"
            );
        }
        let same_name = [valid.clone(), entry("reader", "write", "beta-token")];
        assert_eq!(refused_key(config(&same_name)), "mcp.clients");
        let same_token = [valid.clone(), entry("writer", "write", "alpha-token")];
        assert_eq!(refused_key(config(&same_token)), "mcp.clients");
        let loaded = config(&[valid]).expect("one valid client");
        assert_eq!(loaded.clients[0].scope, Scope::Read);
    }
}
