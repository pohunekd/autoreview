//! The clients of the MCP server: their scopes, token digests and the bearer token check.
//!
//! The configuration holds only the SHA-256 digest of each token. A token is a random secret
//! of at least 32 bytes, so a plain digest cannot be reversed or guessed, and no salt or slow
//! hash is needed.

// Rust guideline compliant 2026-06-26

use std::net::SocketAddr;

use crate::error::Error;

/// Error code of an `mcp` key whose value the loader refuses.
const CONFIGURATION_INVALID: &str = "configuration_invalid";

/// Length of a token digest in bytes.
const DIGEST_BYTES: usize = 32;

/// Longest client name, so that every log line stays short.
const CLIENT_NAME_MAX: usize = 32;

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
