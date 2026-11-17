//! The read of one repository file at a commit, for the repository review rules.
//!
//! One GraphQL query asks for the commit and the file, so a file that is not there is told
//! apart from a commit that the repository does not have. The decode takes the captured `gh`
//! output, so it is a pure function of its arguments.

// Rust guideline compliant 2026-11-17

use autoreview_engine::config::Credential;
use autoreview_engine::error::Error;
use autoreview_engine::integrations::RepositoryFile;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{GitHub, repository_variables, response_invalid};

/// Query of the commit and of one file of it. `file` is `null` when the commit has no such path.
const GRAPHQL_FILE: &str = concat!(
    "query($owner: String!, $name: String!, $oid: GitObjectID!, $expression: String!) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    commit: object(oid: $oid) { oid }\n",
    "    file: object(expression: $expression) {\n",
    "      ... on Blob { byteSize isBinary isTruncated text }\n",
    "    }\n",
    "  }\n",
    "}",
);

/// Returns the `gh` arguments of the file query.
///
/// Every variable is sent as a raw string (`-f`): a typed field (`-F`) would turn a repository
/// named `true`, `null` or `123` into a boolean, null or number.
fn file_args(repository: &str, sha: &str, path: &str) -> Vec<String> {
    let (owner, name) = repository.split_once('/').unwrap_or((repository, ""));
    let mut args = vec!["api".to_owned(), "graphql".to_owned()];
    args.push("-f".to_owned());
    args.push(format!("query={GRAPHQL_FILE}"));
    for (key, value) in [
        ("owner", owner.to_owned()),
        ("name", name.to_owned()),
        ("oid", sha.to_owned()),
        ("expression", format!("{sha}:{path}")),
    ] {
        args.push("-f".to_owned());
        args.push(format!("{key}={value}"));
    }
    args
}

/// Decodes the file query output.
///
/// A response with an error entry or without the repository is `github_response_invalid`. A
/// commit that is not there is the permanent `github_commit_not_found`. A path that is not a
/// regular blob, a binary or truncated blob, a blob without text and a text whose length is not
/// the blob's byte size are `NotText`. The length check detects most lossy decodes, not all: a
/// replacement character that stands for exactly three invalid bytes keeps the length.
fn decode_file(output: &str, max_bytes: u64) -> Result<RepositoryFile, Error> {
    let response: Value =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    let errors = response.get("errors").and_then(Value::as_array);
    if errors.is_some_and(|errors| !errors.is_empty()) {
        return Err(response_invalid(None));
    }
    let repository = response
        .pointer("/data/repository")
        .filter(|node| node.is_object())
        .ok_or_else(|| response_invalid(None))?;
    if !repository.get("commit").is_some_and(Value::is_object) {
        return Err(Error::workflow("github_commit_not_found", true, None));
    }
    let Some(file) = repository.get("file").filter(|file| !file.is_null()) else {
        return Ok(RepositoryFile::Absent);
    };
    let Some(size) = file.get("byteSize").and_then(Value::as_u64) else {
        return Ok(RepositoryFile::NotText);
    };
    if size > max_bytes {
        return Ok(RepositoryFile::TooLarge);
    }
    let flag = |key: &str| file.get(key).and_then(Value::as_bool).unwrap_or(false);
    let text = file.get("text").and_then(Value::as_str);
    match text {
        Some(text) if !flag("isBinary") && !flag("isTruncated") && text.len() as u64 == size => {
            Ok(RepositoryFile::Text(text.to_owned()))
        }
        _ => Ok(RepositoryFile::NotText),
    }
}

/// Query of the blob IDs of two files, each given as `<commit SHA>:<path>`.
///
/// An object that is no blob has no `oid` key.
const GRAPHQL_BLOB_IDS: &str = concat!(
    "query($owner: String!, $name: String!, $first: String!, $second: String!) {\n",
    "  repository(owner: $owner, name: $name) {\n",
    "    first: object(expression: $first) { ... on Blob { oid } }\n",
    "    second: object(expression: $second) { ... on Blob { oid } }\n",
    "  }\n",
    "}",
);

/// Returns the `gh` arguments of the blob ID query; every variable is a raw string (`-f`).
fn blob_ids_args(repository: &str, first: (&str, &str), second: (&str, &str)) -> Vec<String> {
    let mut args = vec!["api".to_owned(), "graphql".to_owned()];
    args.extend(["-f".to_owned(), format!("query={GRAPHQL_BLOB_IDS}")]);
    args.extend(repository_variables(repository));
    for (key, (sha, path)) in [("first", first), ("second", second)] {
        args.extend(["-f".to_owned(), format!("{key}={sha}:{path}")]);
    }
    args
}

/// Decodes the blob ID query output; an object that is no blob has no ID.
fn decode_blob_ids(output: &str) -> Result<[Option<String>; 2], Error> {
    let response: Value =
        serde_json::from_str(output).map_err(|error| response_invalid(Some(Box::new(error))))?;
    let errors = response.get("errors").and_then(Value::as_array);
    if errors.is_some_and(|errors| !errors.is_empty()) {
        return Err(response_invalid(None));
    }
    let repository = response
        .pointer("/data/repository")
        .filter(|node| node.is_object())
        .ok_or_else(|| response_invalid(None))?;
    let id = |key: &str| {
        let oid = repository.pointer(&format!("/{key}/oid"));
        oid.and_then(Value::as_str).map(str::to_owned)
    };
    Ok([id("first"), id("second")])
}

impl GitHub {
    /// Reads the blob IDs of two `(commit SHA, path)` pairs of `repository` with `credential`.
    pub(crate) async fn blob_ids(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        first: (&str, &str),
        second: (&str, &str),
    ) -> Result<[Option<String>; 2], Error> {
        let args = blob_ids_args(repository, first, second);
        let output = self.github::<()>(cancel, credential, &args, None).await?;
        decode_blob_ids(&output)
    }

    /// Reads `path` of `repository` at the commit `sha` with `credential`.
    pub(crate) async fn read_repository_file(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        sha: &str,
        path: &str,
        max_bytes: u64,
    ) -> Result<RepositoryFile, Error> {
        let args = file_args(repository, sha, path);
        let output = self.github::<()>(cancel, credential, &args, None).await?;
        decode_file(&output, max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use autoreview_engine::error_code;
    use autoreview_engine::integrations::RepositoryFile;
    use serde_json::json;

    use super::{decode_file, file_args};

    /// Returns the answer for a commit and a file node.
    fn answer(commit: serde_json::Value, file: serde_json::Value) -> String {
        json!({"data": {"repository": {"commit": commit, "file": file}}}).to_string()
    }

    /// Returns a file node of the given size, flags and text.
    fn blob(
        size: u64,
        binary: bool,
        truncated: bool,
        text: serde_json::Value,
    ) -> serde_json::Value {
        json!({"byteSize": size, "isBinary": binary, "isTruncated": truncated, "text": text})
    }

    /// Each row is a GraphQL answer and the file it reads with a limit of 100 bytes, or the code.
    #[test]
    fn a_missing_file_is_told_apart_from_a_missing_commit_and_a_lossy_text() {
        let oid = json!({"oid": "abc"});
        let cases: Vec<(&str, String, Result<RepositoryFile, &str>)> = vec![
            (
                "text",
                answer(oid.clone(), blob(5, false, false, json!("rules"))),
                Ok(RepositoryFile::Text("rules".to_owned())),
            ),
            (
                "unknown encoding",
                answer(
                    oid.clone(),
                    json!({"byteSize": 2, "isBinary": null, "isTruncated": false, "text": "ok"}),
                ),
                Ok(RepositoryFile::Text("ok".to_owned())),
            ),
            (
                "no file",
                answer(oid.clone(), json!(null)),
                Ok(RepositoryFile::Absent),
            ),
            (
                "too large",
                answer(oid.clone(), blob(101, false, false, json!(null))),
                Ok(RepositoryFile::TooLarge),
            ),
            (
                "binary",
                answer(oid.clone(), blob(5, true, false, json!(null))),
                Ok(RepositoryFile::NotText),
            ),
            (
                "truncated",
                answer(oid.clone(), blob(5, false, true, json!("rule"))),
                Ok(RepositoryFile::NotText),
            ),
            (
                "lossy decode",
                answer(oid.clone(), blob(4, false, false, json!("\u{fffd}"))),
                Ok(RepositoryFile::NotText),
            ),
            (
                "null text",
                answer(oid.clone(), blob(5, false, false, json!(null))),
                Ok(RepositoryFile::NotText),
            ),
            (
                "a directory",
                answer(oid, json!({})),
                Ok(RepositoryFile::NotText),
            ),
            (
                "no commit",
                answer(json!(null), json!(null)),
                Err("github_commit_not_found"),
            ),
            (
                "no repository",
                json!({"data": {"repository": null}}).to_string(),
                Err("github_response_invalid"),
            ),
            (
                "an error entry",
                json!({"data": null, "errors": [{"type": "FORBIDDEN"}]}).to_string(),
                Err("github_response_invalid"),
            ),
        ];
        for (name, output, want) in cases {
            let got = decode_file(&output, 100).map_err(|err| error_code(&err).to_owned());
            assert_eq!(got, want.map_err(str::to_owned), "{name}");
        }
        // A repository named like a JSON literal is still sent as a string.
        let args = file_args("true/123", "abc", "REVIEW.md");
        assert!(!args.iter().any(|arg| arg == "-F"), "args = {args:?}");
        assert!(args.contains(&"owner=true".to_owned()), "args = {args:?}");
    }
}
