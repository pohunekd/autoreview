//! The repository permission of a pull request author, read with the serving credential.

// Rust guideline compliant 2026-06-26

use autoreview_engine::error::Error;
use autoreview_engine::model::repository_valid;
use autoreview_go_compat::null_default;
use autoreview_policy::{Permission, user_login_valid};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{GitHub, response_invalid};

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
    /// Returns `github_request_invalid` for an invalid repository or login,
    /// `credential_not_covered` when no credential serves the repository, the error of the `gh`
    /// call, and `github_response_invalid` when its output holds no role.
    pub(crate) async fn author_permission(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        login: &str,
    ) -> Result<Option<Permission>, Error> {
        if !repository_valid(repository) || !user_login_valid(login) {
            return Err(Error::workflow("github_request_invalid", true, None));
        }
        let credential = self.credentials.serving(repository)?;
        let args = permission_args(repository, login);
        let output = self.github::<()>(cancel, credential, &args, None).await?;
        decode_permission(&output)
    }
}
