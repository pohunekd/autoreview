//! The clients of the MCP server: their scopes, token digests and the bearer token check.
//!
//! The configuration holds only the SHA-256 digest of each token. A token is a random secret
//! of at least 32 bytes, so a plain digest cannot be reversed or guessed, and no salt or slow
//! hash is needed.

// Rust guideline compliant 2026-06-26

use std::net::SocketAddr;
use std::time::Duration;

use http::uri::Authority;
use sha2::{Digest, Sha256};

use crate::error::Error;

/// Error code of an `mcp` key whose value the loader refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Length of a token digest in bytes.
const DIGEST_BYTES: usize = 32;

/// Longest client name, so that every log line stays short.
const CLIENT_NAME_MAX: usize = 32;

/// Longest request or header timeout: one day, far below the overflow of `Instant`.
pub(crate) const TIMEOUT_MAX: Duration = Duration::from_secs(86_400);

/// Largest request body: 16 MiB, far above any tool's arguments.
const REQUEST_MAX_BYTES_LIMIT: usize = 16 << 20;

/// Most connections at once, well below the permit limit of the connection semaphore.
pub(crate) const MAX_CONNECTIONS_LIMIT: usize = 4096;

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

/// Limits of the HTTP server, from the `mcp` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpLimits {
    /// Longest time one request may take, from reading its body to its answer.
    pub request_timeout: Duration,
    /// Longest time a client may take to send the request headers.
    pub header_timeout: Duration,
    /// Largest request body in bytes.
    pub request_max_bytes: usize,
    /// Most connections served at once; further connections wait to be accepted.
    pub max_connections: usize,
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
    /// Limits of the HTTP server.
    pub limits: McpLimits,
}

impl McpConfig {
    /// Validates the `mcp` section and builds the server settings.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` naming `mcp.listen` for an address that does not
    /// parse, `mcp.allowed_hosts` for an empty list or an entry that is not a bare host or
    /// `host:port`, `mcp.clients` for no client, `mcp.clients[N].name`, `.scope` or
    /// `.token_sha256` for a bad or repeated value of client `N`, and the key of a limit that
    /// is zero or above its bound.
    pub fn new(
        listen: &str,
        allowed_hosts: Vec<String>,
        entries: &[McpClientEntry],
        limits: McpLimits,
    ) -> Result<Self, Error> {
        let listen = listen
            .parse::<SocketAddr>()
            .map_err(|_| invalid("mcp.listen", "must be an IP address and port"))?;
        check_limits(&limits)?;
        if allowed_hosts.is_empty() || !allowed_hosts.iter().all(|host| host_valid(host)) {
            return Err(invalid(
                "mcp.allowed_hosts",
                "must list hosts or host:port, without scheme, path or user",
            ));
        }
        if entries.is_empty() {
            return Err(invalid("mcp.clients", "must list at least one client"));
        }
        let mut clients: Vec<McpClient> = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let client = client(index, entry)?;
            if clients.iter().any(|known| known.name == client.name) {
                return Err(invalid(
                    format!("mcp.clients[{index}].name"),
                    "client names must be unique",
                ));
            }
            if clients
                .iter()
                .any(|known| known.token_sha256 == client.token_sha256)
            {
                return Err(invalid(
                    format!("mcp.clients[{index}].token_sha256"),
                    "token digests must be unique",
                ));
            }
            clients.push(client);
        }
        Ok(Self {
            listen,
            allowed_hosts,
            clients,
            limits,
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

/// Refuses a limit that is zero or above its bound, naming its key.
fn check_limits(limits: &McpLimits) -> Result<(), Error> {
    let timeouts = [
        ("mcp.request_timeout_seconds", limits.request_timeout),
        ("mcp.header_timeout_seconds", limits.header_timeout),
    ];
    for (key, timeout) in timeouts {
        if timeout.is_zero() || timeout > TIMEOUT_MAX {
            return Err(invalid(key, "must be from 1 to 86400 seconds"));
        }
    }
    if !(1..=REQUEST_MAX_BYTES_LIMIT).contains(&limits.request_max_bytes) {
        return Err(invalid(
            "mcp.request_max_bytes",
            "must be from 1 to 16777216 bytes",
        ));
    }
    if !(1..=MAX_CONNECTIONS_LIMIT).contains(&limits.max_connections) {
        return Err(invalid("mcp.max_connections", "must be from 1 to 4096"));
    }
    Ok(())
}

/// Reports whether `host` is a bare `host` or `host:port` authority, as a `Host` header is.
pub(crate) fn host_valid(host: &str) -> bool {
    // `Authority` keeps a port it cannot read as `u16`, so a port must also parse.
    !host.contains('@')
        && host.parse::<Authority>().is_ok_and(|authority| {
            authority.as_str() == host
                && (authority.host() == host || authority.port_u16().is_some())
        })
}

/// Validates the client entry at `index`.
fn client(index: usize, entry: &McpClientEntry) -> Result<McpClient, Error> {
    let name_valid = !entry.name.is_empty()
        && entry.name.len() <= CLIENT_NAME_MAX
        && entry
            .name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !name_valid {
        return Err(invalid(
            format!("mcp.clients[{index}].name"),
            "a name has 1 to 32 lowercase letters, digits or hyphens",
        ));
    }
    let Some(scope) = Scope::parse(&entry.scope) else {
        return Err(invalid(
            format!("mcp.clients[{index}].scope"),
            "a scope is read or write",
        ));
    };
    let lower = entry
        .token_sha256
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let decoded = hex::decode(&entry.token_sha256).ok().filter(|_| lower);
    let Some(token_sha256) = decoded.and_then(|bytes| <[u8; DIGEST_BYTES]>::try_from(bytes).ok())
    else {
        return Err(invalid(
            format!("mcp.clients[{index}].token_sha256"),
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
pub(crate) fn invalid(key: impl Into<String>, message: &'static str) -> Error {
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

    fn limits() -> McpLimits {
        McpLimits {
            request_timeout: Duration::from_secs(60),
            header_timeout: Duration::from_secs(10),
            request_max_bytes: 65_536,
            max_connections: 16,
        }
    }

    fn config(entries: &[McpClientEntry]) -> Result<McpConfig, Error> {
        hosts_config(&["mcp.example.com"], entries)
    }

    fn hosts_config(hosts: &[&str], entries: &[McpClientEntry]) -> Result<McpConfig, Error> {
        let hosts = hosts.iter().map(|host| (*host).to_owned()).collect();
        McpConfig::new("127.0.0.1:8787", hosts, entries, limits())
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
        let listen = McpConfig::new("localhost:8787", vec!["h".to_owned()], &[], limits());
        assert_eq!(refused_key(listen), "mcp.listen");
        let day = Duration::from_secs(86_400);
        let limit_cases = [
            (
                McpLimits {
                    request_timeout: day + Duration::from_secs(1),
                    ..limits()
                },
                "mcp.request_timeout_seconds",
            ),
            (
                McpLimits {
                    header_timeout: Duration::from_secs(u64::MAX),
                    ..limits()
                },
                "mcp.header_timeout_seconds",
            ),
            (
                McpLimits {
                    header_timeout: Duration::ZERO,
                    ..limits()
                },
                "mcp.header_timeout_seconds",
            ),
            (
                McpLimits {
                    request_max_bytes: (16 << 20) + 1,
                    ..limits()
                },
                "mcp.request_max_bytes",
            ),
            (
                McpLimits {
                    max_connections: usize::MAX,
                    ..limits()
                },
                "mcp.max_connections",
            ),
            (
                McpLimits {
                    max_connections: 0,
                    ..limits()
                },
                "mcp.max_connections",
            ),
        ];
        for (bounds, key) in limit_cases {
            let hosts = vec!["h".to_owned()];
            let result = McpConfig::new(
                "127.0.0.1:8787",
                hosts,
                std::slice::from_ref(&valid),
                bounds,
            );
            assert_eq!(refused_key(result), key, "limits {bounds:?}");
        }
        let edge = McpLimits {
            request_timeout: day,
            header_timeout: day,
            request_max_bytes: 16 << 20,
            max_connections: 4096,
        };
        let hosts = vec!["h".to_owned()];
        let edges = McpConfig::new("127.0.0.1:8787", hosts, std::slice::from_ref(&valid), edge);
        assert!(
            edges.is_ok(),
            "every limit at its bound is accepted: {edges:?}"
        );
        let host_cases: [&[&str]; 8] = [
            &[],
            &[""],
            &["a b"],
            &["https://mcp.example.com"],
            &["mcp.example.com/mcp"],
            &["user@mcp.example.com"],
            &["mcp.example.com:99999"],
            &["mcp.example.com", "mcp.example.com?x"],
        ];
        for hosts in host_cases {
            let result = hosts_config(hosts, std::slice::from_ref(&valid));
            assert_eq!(refused_key(result), "mcp.allowed_hosts", "hosts {hosts:?}");
        }
        let hosts = ["mcp.example.com", "127.0.0.1:8443", "[::1]:8443"];
        let accepted = hosts_config(&hosts, std::slice::from_ref(&valid));
        assert!(
            accepted.is_ok(),
            "bare hosts and host:port are accepted: {accepted:?}"
        );
        let client_cases = [
            (Vec::new(), "mcp.clients"),
            (vec![entry("Reader", "read", "x")], "mcp.clients[0].name"),
            (
                vec![valid.clone(), entry("w", "admin", "x")],
                "mcp.clients[1].scope",
            ),
            (vec![upper], "mcp.clients[0].token_sha256"),
            (vec![short], "mcp.clients[0].token_sha256"),
            (
                vec![valid.clone(), entry("reader", "write", "beta-token")],
                "mcp.clients[1].name",
            ),
            (
                vec![valid.clone(), entry("writer", "write", "alpha-token")],
                "mcp.clients[1].token_sha256",
            ),
        ];
        for (entries, key) in client_cases {
            assert_eq!(refused_key(config(&entries)), key, "entries {entries:?}");
        }
        let loaded = config(&[valid]).expect("one valid client");
        assert_eq!(loaded.clients[0].scope, Scope::Read);
    }
}
