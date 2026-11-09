//! The repository permission of a pull request author, read with the serving credential.

// Rust guideline compliant 2026-06-26

use autoreview_engine::error::Error;
use autoreview_engine::model::repository_valid;
use autoreview_go_compat::null_default;
use autoreview_policy::Permission;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{GitHub, response_invalid};

/// Longest login in bytes that GitHub allows.
const MAX_LOGIN_BYTES: usize = 39;

/// The response of `gh api repos/<repository>/collaborators/<login>/permission`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, remote = "Self")]
struct PermissionResponse {
    /// Role of the login in the repository, custom roles included.
    ///
    /// The legacy `permission` field has no maintain, so only the role is read.
    #[serde(deserialize_with = "null_default::deserialize")]
    role_name: String,
}

autoreview_go_compat::object_only_serde!(PermissionResponse);

/// Reports whether `login` is a plain GitHub user login, safe to place in an API path.
///
/// An Enterprise Managed User login carries an underscore (`handle_shortcode`). Letters,
/// digits, `-` and `_` are unreserved in a URL path, so the login needs no encoding.
fn login_valid(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= MAX_LOGIN_BYTES
        && login
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Returns the argv of the permission read of `login` in `repository`.
fn permission_args(repository: &str, login: &str) -> Vec<String> {
    vec![
        "api".to_owned(),
        format!("repos/{repository}/collaborators/{login}/permission"),
    ]
}

/// Decodes the permission of a response, `None` below write; an empty role is refused.
fn decode_permission(output: &str) -> Result<Option<Permission>, Error> {
    let response: PermissionResponse =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    if response.role_name.is_empty() {
        return Err(response_invalid(None));
    }
    Ok(Permission::from_role(&response.role_name))
}

impl GitHub {
    /// Returns the repository permission of `login`, `None` below write.
    ///
    /// # Errors
    ///
    /// Returns `github_request_invalid` for an invalid repository or login, the error of the
    /// `gh` call, and `github_response_invalid` when its output holds no role.
    pub(crate) async fn author_permission(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        login: &str,
    ) -> Result<Option<Permission>, Error> {
        if !repository_valid(repository) || !login_valid(login) {
            return Err(Error::workflow("github_request_invalid", true, None));
        }
        let output = self
            .github::<()>(cancel, &permission_args(repository, login), None)
            .await?;
        decode_permission(&output)
    }
}

#[cfg(test)]
mod tests {
    use autoreview_engine::error_code;
    use autoreview_policy::Permission;

    use super::{decode_permission, login_valid};

    #[test]
    fn permission_is_read_from_the_role_name() {
        let cases = [
            (
                r#"{"permission":"write","role_name":"maintain"}"#,
                Some(Permission::Maintain),
            ),
            (
                r#"{"permission":"admin","role_name":"admin"}"#,
                Some(Permission::Admin),
            ),
            (
                r#"{"permission":"write","role_name":"write"}"#,
                Some(Permission::Write),
            ),
            (r#"{"permission":"read","role_name":"triage"}"#, None),
            (
                r#"{"permission":"write","role_name":"security-reviewer"}"#,
                None,
            ),
        ];
        for (output, want) in cases {
            match decode_permission(output) {
                Ok(permission) => assert_eq!(permission, want, "{output}"),
                Err(error) => panic!("{output}: {error}"),
            }
        }
        for output in [r#"{"permission":"admin"}"#, r#"{"role_name":null}"#, "[]"] {
            let code = decode_permission(output).map_err(|error| error_code(&error).to_owned());
            assert_eq!(
                code,
                Err("github_response_invalid".to_owned()),
                "{output} must be refused, never read as a permission"
            );
        }
        assert!(
            login_valid("handle_shortcode")
                && !login_valid("bot[bot]")
                && !login_valid("a/b")
                && !login_valid("a%2fb"),
            "an Enterprise Managed User login is read, and no path character passes"
        );
    }
}
