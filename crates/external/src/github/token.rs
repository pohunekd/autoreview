//! The token check of one credential: `gh api --include user`, its status and its headers.

// Rust guideline compliant 2026-11-13

use autoreview_engine::config::Credential;
use autoreview_engine::error::Error;
use autoreview_engine::integrations::TokenAnswer;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};
use tokio_util::sync::CancellationToken;

use super::{GitHub, decode_viewer_login, gh_failure, response_invalid};
use crate::process::run;

/// Response header that carries the token's expiry; names ignore case.
const EXPIRATION_HEADER: &str = "GitHub-Authentication-Token-Expiration";

/// HTTP status of a token that GitHub refuses.
const UNAUTHORIZED: u16 = 401;

/// HTTP status of an accepted `GET /user`.
const OK: u16 = 200;

/// Key named when the expiry of a token is not a time.
const CREDENTIALS_KEY: &str = "github.credentials";

impl GitHub {
    /// Asks GitHub whom the token of `credential` belongs to and when it expires.
    ///
    /// # Errors
    ///
    /// Returns the error of the `gh` call unless GitHub answered 401, and
    /// `github_response_invalid` when the answer holds no login or a malformed expiry.
    pub(crate) async fn check_token(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
    ) -> Result<TokenAnswer, Error> {
        let args = ["api", "--include", "user"].map(str::to_owned);
        let environment = self.environment(std::env::vars_os(), credential);
        let result = run(cancel, &self.binary, &args, &[], &environment, self.timeout).await;
        match result {
            Ok(output) => decode_token_answer(&credential.name, &output),
            Err(error) => {
                // gh fails on a 401 and still prints the status line with `--include`.
                if status(error.stdout()) == Some(UNAUTHORIZED) {
                    return Ok(TokenAnswer::Rejected);
                }
                let (code, permanent) = gh_failure(&error.to_string());
                Err(Error::workflow(code, permanent, Some(Box::new(error))))
            }
        }
    }
}

/// Returns the HTTP status of the status line that starts `output`, as `HTTP/2.0 401 ...`.
fn status(output: &str) -> Option<u16> {
    let line = output.lines().next()?;
    let (protocol, rest) = line.split_once(' ')?;
    if !protocol.starts_with("HTTP/") {
        return None;
    }
    rest.split(' ').next()?.parse().ok()
}

/// Decodes the `--include` output of an accepted `GET /user` of the credential `name`.
///
/// The output is the status line, the header lines, an empty line and the JSON body.
fn decode_token_answer(name: &str, output: &str) -> Result<TokenAnswer, Error> {
    if status(output) != Some(OK) {
        return Err(response_invalid(None));
    }
    let mut lines = output.split_inclusive('\n');
    let mut expires_at = None;
    let mut head = 0;
    for line in lines.by_ref() {
        head += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        if field.eq_ignore_ascii_case(EXPIRATION_HEADER) {
            expires_at = Some(expiration(name, value.trim())?);
        }
    }
    let body = output.get(head..).ok_or_else(|| response_invalid(None))?;
    let login = decode_viewer_login(body)?;
    Ok(TokenAnswer::Accepted { login, expires_at })
}

/// Parses the expiry header `value` of the credential `name`.
///
/// The time is `2026-12-31 23:59:59 UTC`, or with a numeric offset such as `-0800`.
///
/// # Errors
///
/// Returns `github_response_invalid` naming `github.credentials`; the message names the
/// credential and the value.
fn expiration(name: &str, value: &str) -> Result<OffsetDateTime, Error> {
    let utc = format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
    let offset = format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]"
    );
    PrimitiveDateTime::parse(value, utc)
        .map(PrimitiveDateTime::assume_utc)
        .or_else(|_| OffsetDateTime::parse(value, offset))
        .map_err(|_| {
            let message = format!("the token expiry {value:?} of credential {name} is not a time");
            Error::config_with_message("github_response_invalid", CREDENTIALS_KEY, message, None)
        })
}

#[cfg(test)]
mod tests {
    use autoreview_go_compat::go_time::format_rfc3339_seconds;

    use super::*;

    /// The `--include` output of gh for an accepted `GET /user` with the header lines `headers`.
    ///
    /// gh ends the status line with `\n` and every header line with `\r\n`.
    fn included(headers: &[&str]) -> String {
        let lines: String = headers.iter().map(|line| format!("{line}\r\n")).collect();
        format!("HTTP/2.0 200 OK\n{lines}\r\n{{\"login\":\"Review-Bot\"}}")
    }

    /// Renders a decoded answer as `login expiry`, or as the error code and message.
    fn render(result: Result<TokenAnswer, Error>) -> String {
        match result {
            Ok(TokenAnswer::Accepted { login, expires_at }) => {
                let expires_at = expires_at.map_or("null".to_owned(), format_rfc3339_seconds);
                format!("{login} {expires_at}")
            }
            Ok(TokenAnswer::Rejected) => "rejected".to_owned(),
            Err(Error::Config { code, message, .. }) => {
                format!("{code}: {}", message.unwrap_or_default())
            }
            Err(other) => autoreview_engine::error_code(&other).to_owned(),
        }
    }

    #[test]
    fn token_answer_reads_the_login_and_the_expiry_header() {
        let json = "Content-Type: application/json; charset=utf-8";
        let expiry = |value: &str| {
            included(&[
                json,
                &format!("Github-Authentication-Token-Expiration: {value}"),
            ])
        };
        let not_a_time = |value: &str| {
            let message = format!("the token expiry {value:?} of credential default");
            format!("github_response_invalid: {message} is not a time")
        };
        let lower = "github-authentication-token-expiration: 2026-12-31 23:59:59 UTC";
        let at = "review-bot 2026-12-31T23:59:59Z".to_owned();
        let invalid = "github_response_invalid".to_owned();
        let cases = [
            ("UTC", expiry("2026-12-31 23:59:59 UTC"), at.clone()),
            ("offset", expiry("2026-12-31 15:59:59 -0800"), at.clone()),
            ("absent", included(&[json]), "review-bot null".to_owned()),
            ("lower-case name", included(&[lower]), at),
            (
                "RFC 3339",
                expiry("2026-12-31T23:59:59Z"),
                not_a_time("2026-12-31T23:59:59Z"),
            ),
            (
                "zone name",
                expiry("2026-12-31 15:59:59 PST"),
                not_a_time("2026-12-31 15:59:59 PST"),
            ),
            (
                "no date",
                expiry("2026-02-30 23:59:59 UTC"),
                not_a_time("2026-02-30 23:59:59 UTC"),
            ),
            ("empty", expiry(""), not_a_time("")),
            (
                "no status line",
                "{\"login\":\"review-bot\"}".to_owned(),
                invalid.clone(),
            ),
            ("no body", "HTTP/2.0 200 OK\n\r\n".to_owned(), invalid),
        ];
        for (case, output, expected) in cases {
            assert_eq!(
                render(decode_token_answer("default", &output)),
                expected,
                "{case}"
            );
        }
        let statuses = [
            (
                "HTTP/2.0 401 Unauthorized\nContent-Length: 2\r\n\r\n{}",
                Some(UNAUTHORIZED),
            ),
            (
                "HTTP/1.1 403 Forbidden\nContent-Length: 2\r\n\r\n{}",
                Some(403),
            ),
            ("gh: Bad credentials (HTTP 401)", None),
            ("", None),
        ];
        for (output, expected) in statuses {
            assert_eq!(status(output), expected, "{output}");
        }
    }
}
