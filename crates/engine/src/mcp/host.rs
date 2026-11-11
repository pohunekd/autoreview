//! The `Host` check that the MCP server and the metrics endpoint share.
//!
//! It applies the rules of the DNS rebinding check of rmcp 3.9.0, so both listeners accept the
//! same requests. The MCP server keeps rmcp's own check behind it as a second line.

// Rust guideline compliant 2026-06-26

use std::borrow::Cow;

use http::header::HOST;
use http::uri::Authority;
use http::{HeaderMap, HeaderValue, StatusCode, Uri};

/// A host and an optional port, the host lowercased and without IPv6 brackets.
type HostPort = (String, Option<u16>);

/// The most bytes of the host that a refusal log line takes before lossy decoding.
///
/// A valid `host:port` has at most 259 bytes (a DNS name of 253 bytes and `:65535`), so only a
/// host that cannot be a real name is cut.
const LOGGED_HOST_MAX_BYTES: usize = 260;

/// Why the `Host` of a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostRefusal {
    /// No host at all, or a `Host` header that is not an authority.
    Invalid,
    /// A host that no `allowed_hosts` entry accepts.
    NotAllowed,
}

impl HostRefusal {
    /// Returns the reason of the refusal, as the logs write it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "host_invalid",
            Self::NotAllowed => "host_not_allowed",
        }
    }

    /// Returns the status of the answer: 400 for an invalid host, 403 for another host.
    pub(crate) fn status(self) -> StatusCode {
        match self {
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::NotAllowed => StatusCode::FORBIDDEN,
        }
    }
}

/// The `Host` values a listener accepts; an entry without a port accepts every port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AllowedHosts {
    /// The entries of `allowed_hosts`, lowercased and without IPv6 brackets.
    entries: Vec<HostPort>,
}

impl AllowedHosts {
    /// Reads the `allowed_hosts` entries; an entry that is no authority accepts nothing.
    pub(crate) fn new(entries: &[String]) -> Self {
        Self {
            entries: entries
                .iter()
                .filter_map(|entry| entry.parse::<Authority>().ok())
                .map(|authority| normalize(&authority))
                .collect(),
        }
    }

    /// Checks the host of a request: the first `Host` header, else the target's authority.
    ///
    /// The fallback serves a request target in absolute form (`GET http://host/path`) that
    /// carries no `Host` header, as rmcp does.
    ///
    /// # Errors
    ///
    /// Returns `Invalid` for a `Host` header that is not UTF-8 or not an authority, or for no
    /// header and no authority in the target, and `NotAllowed` for a host no entry accepts.
    pub(crate) fn check(&self, uri: &Uri, headers: &HeaderMap) -> Result<(), HostRefusal> {
        let (host, port) = read_host(uri, headers)?;
        let allowed = self.entries.iter().any(|(allowed, allowed_port)| {
            *allowed == host && allowed_port.is_none_or(|allowed_port| port == Some(allowed_port))
        });
        if allowed {
            Ok(())
        } else {
            Err(HostRefusal::NotAllowed)
        }
    }
}

/// Returns the host that [`AllowedHosts::check`] reads from a request, for its refusal log line.
///
/// A host that parses is written as the check compares it: lowercased, IPv6 in brackets, with
/// its port if it has one. Otherwise it is the raw `Host` header as lossy UTF-8, or empty
/// without a header. Either is cut to its first [`LOGGED_HOST_MAX_BYTES`] bytes before lossy
/// decoding, which writes each invalid UTF-8 sequence as U+FFFD (three bytes).
pub(crate) fn logged_host(uri: &Uri, headers: &HeaderMap) -> String {
    let read: Cow<'_, [u8]> = match read_host(uri, headers) {
        Ok((host, port)) => {
            let host = if host.contains(':') {
                format!("[{host}]")
            } else {
                host
            };
            let text = match port {
                Some(port) => format!("{host}:{port}"),
                None => host,
            };
            Cow::Owned(text.into_bytes())
        }
        Err(_) => Cow::Borrowed(headers.get(HOST).map_or(&[], HeaderValue::as_bytes)),
    };
    let cut = read.get(..LOGGED_HOST_MAX_BYTES).unwrap_or(&read);
    String::from_utf8_lossy(cut).into_owned()
}

/// Reads the host of a request: the first `Host` header, else the target's authority.
///
/// # Errors
///
/// Returns `Invalid` for a `Host` header that is not UTF-8 or not an authority, or for no
/// header and no authority in the target.
fn read_host(uri: &Uri, headers: &HeaderMap) -> Result<HostPort, HostRefusal> {
    match headers.get(HOST) {
        Some(value) => {
            let text = value.to_str().map_err(|_| HostRefusal::Invalid)?;
            let authority = text
                .parse::<Authority>()
                .map_err(|_| HostRefusal::Invalid)?;
            Ok(normalize(&authority))
        }
        None => Ok(normalize(uri.authority().ok_or(HostRefusal::Invalid)?)),
    }
}

/// Returns the host of `authority`, lowercased and without IPv6 brackets, and its port.
///
/// A port that is no `u16` counts as no port, as in rmcp.
fn normalize(authority: &Authority) -> HostPort {
    let host = authority
        .host()
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase();
    (host, authority.port_u16())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request: its target, its `Host` header values in order, and the expected result.
    type Case = (
        &'static str,
        &'static [&'static [u8]],
        Result<(), HostRefusal>,
    );

    /// Returns a request target and its `Host` header values in order.
    fn request(target: &str, hosts: &[&[u8]]) -> (Uri, HeaderMap) {
        let uri = target.parse::<Uri>().expect("the target parses");
        let mut headers = HeaderMap::new();
        for host in hosts {
            let value = HeaderValue::from_bytes(host).expect("the header value is valid");
            headers.append(HOST, value);
        }
        (uri, headers)
    }

    /// Each row is the answer of the MCP server with rmcp 3.9.0 alone, which the check keeps.
    #[test]
    fn host_check_accepts_and_refuses_as_rmcp_does() {
        let entries = [
            "mcp.example.com",
            "127.0.0.1:8443",
            "[::1]",
            "UPPER.example.com",
        ];
        let allowed = AllowedHosts::new(&entries.map(str::to_owned));
        let bad = Err(HostRefusal::Invalid);
        let other = Err(HostRefusal::NotAllowed);
        let cases: [Case; 31] = [
            ("/metrics", &[b"mcp.example.com"], Ok(())),
            ("/metrics", &[b"mcp.example.com:9999"], Ok(())),
            ("/metrics", &[b"MCP.Example.COM"], Ok(())),
            ("/metrics", &[b"upper.example.com"], Ok(())),
            ("/metrics", &[b"mcp.example.com."], other),
            ("/metrics", &[b"other.example.com"], other),
            ("/metrics", &[b""], bad),
            ("/metrics", &[], bad),
            ("http://mcp.example.com/metrics", &[], Ok(())),
            ("http://other.example.com/metrics", &[], other),
            (
                "http://other.example.com/metrics",
                &[b"mcp.example.com"],
                Ok(()),
            ),
            (
                "http://mcp.example.com/metrics",
                &[b"other.example.com"],
                other,
            ),
            ("/metrics", &[b"127.0.0.1:8443"], Ok(())),
            ("/metrics", &[b"127.0.0.1"], other),
            ("/metrics", &[b"127.0.0.1:8444"], other),
            ("/metrics", &[b"[::1]"], Ok(())),
            ("/metrics", &[b"[::1]:5"], Ok(())),
            ("/metrics", &[b"[::ABCD]"], other),
            ("/metrics", &[b"::1"], bad),
            ("/metrics", &[b"[::1"], bad),
            ("/metrics", &[b"user@mcp.example.com"], Ok(())),
            ("/metrics", &[b"mcp.example.com:99999"], Ok(())),
            ("/metrics", &[b"mcp.example.com:"], Ok(())),
            ("/metrics", &[b"mcp.example.com/x"], bad),
            ("/metrics", &[b"a b"], bad),
            ("/metrics", &[b"caf\xe9.example.com"], bad),
            (
                "/metrics",
                &[b"mcp.example.com", b"other.example.com"],
                Ok(()),
            ),
            (
                "/metrics",
                &[b"other.example.com", b"mcp.example.com"],
                other,
            ),
            ("/metrics", &[b" mcp.example.com"], bad),
            ("/metrics", &[b"[::1]:8443"], Ok(())),
            ("/metrics", &[b"127.0.0.1:99999"], other),
        ];
        for (target, hosts, expected) in cases {
            let (uri, headers) = request(target, hosts);
            let found = allowed.check(&uri, &headers);
            assert_eq!(found, expected, "target {target}, hosts {hosts:?}");
        }
        let long = "x ".repeat(LOGGED_HOST_MAX_BYTES);
        let long_name = "x".repeat(2 * LOGGED_HOST_MAX_BYTES);
        let logged: [(&str, &[&[u8]], &str); 6] = [
            ("/metrics", &[b"127.0.0.1:8444"], "127.0.0.1:8444"),
            ("/metrics", &[b"[::ABCD]:5"], "[::abcd]:5"),
            (
                "/metrics",
                &[b"caf\xe9.example.com"],
                "caf\u{fffd}.example.com",
            ),
            (
                "/metrics",
                &[long.as_bytes()],
                &long[..LOGGED_HOST_MAX_BYTES],
            ),
            (
                "/metrics",
                &[long_name.as_bytes()],
                &long_name[..LOGGED_HOST_MAX_BYTES],
            ),
            ("/metrics", &[], ""),
        ];
        for (target, hosts, expected) in logged {
            let (uri, headers) = request(target, hosts);
            assert!(allowed.check(&uri, &headers).is_err(), "hosts {hosts:?}");
            let found = logged_host(&uri, &headers);
            assert_eq!(found, expected, "target {target}, hosts {hosts:?}");
        }
        let hex = AllowedHosts::new(&["[::abcd]".to_owned()]);
        let uri = Uri::from_static("/metrics");
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("[::ABCD]:80"));
        assert_eq!(
            hex.check(&uri, &headers),
            Ok(()),
            "IPv6 hex digits ignore case"
        );
        let none = AllowedHosts::new(&[]);
        headers.insert(HOST, HeaderValue::from_static("mcp.example.com"));
        assert_eq!(
            none.check(&uri, &headers),
            other,
            "an empty list accepts no host; the config refuses one"
        );
    }
}
