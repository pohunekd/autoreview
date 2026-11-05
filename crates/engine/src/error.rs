//! Engine errors and the stable codes they carry.
//!
//! A code is a string that is published in JSON command errors and stored in the
//! `failure` column, so the strings are part of the contract with the Go reference.

// Rust guideline compliant 2026-06-26

use std::error::Error as StdError;

/// Boxed error that can be the cause of an engine error.
///
/// Go's `error` fields hold any error, so a cause may come from outside the engine.
pub type BoxError = Box<dyn StdError + Send + Sync + 'static>;

/// Code of a workflow failure that carries no code of its own.
pub const WORKFLOW_FAILED: &str = "workflow_failed";

/// Errors of the review engine. Callers match on the code, never on the message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A workflow step failed.
    ///
    /// `permanent` tells the retry logic that retrying cannot succeed.
    #[error("{}", with_cause(code, cause.as_ref()))]
    Workflow {
        /// The stable code, for example `github_request_failed`.
        code: String,
        /// Whether retrying cannot succeed.
        permanent: bool,
        /// The error that caused this failure, if any.
        #[source]
        cause: Option<BoxError>,
    },
    /// A configuration key is missing or invalid.
    #[error("{}", with_cause(&format!("{code}: {name}"), cause.as_ref()))]
    Config {
        /// The stable code, for example `configuration_invalid`.
        code: String,
        /// The name of the key that failed, for example `AUTOREVIEW_POLICY_FILE`.
        name: String,
        /// The error that caused this failure, if any.
        #[source]
        cause: Option<BoxError>,
    },
    /// A pohunek CLI call failed and reported an error code.
    #[error("{cause}: pohunek error code {code}")]
    PohunekCode {
        /// The error code from the CLI envelope.
        code: String,
        /// The error of the failed call.
        #[source]
        cause: BoxError,
    },
}

impl Error {
    /// Creates a workflow failure with `code`.
    pub fn workflow(code: impl Into<String>, permanent: bool, cause: Option<BoxError>) -> Self {
        Self::Workflow {
            code: code.into(),
            permanent,
            cause,
        }
    }

    /// Creates a configuration error for the key `name`.
    pub fn config(
        code: impl Into<String>,
        name: impl Into<String>,
        cause: Option<BoxError>,
    ) -> Self {
        Self::Config {
            code: code.into(),
            name: name.into(),
            cause,
        }
    }

    /// Wraps the error of a failed pohunek CLI call together with the code it reported.
    pub fn pohunek_code(code: impl Into<String>, cause: BoxError) -> Self {
        Self::PohunekCode {
            code: code.into(),
            cause,
        }
    }
}

/// Renders `head`, followed by `: cause` when there is a cause.
fn with_cause(head: &str, cause: Option<&BoxError>) -> String {
    match cause {
        None => head.to_string(),
        Some(c) => format!("{head}: {c}"),
    }
}

fn engine_errors<'a>(err: &'a (dyn StdError + 'static)) -> impl Iterator<Item = &'a Error> + 'a {
    std::iter::successors(Some(err as &(dyn StdError + 'static)), |&current| {
        current.source()
    })
    .filter_map(|err| err.downcast_ref::<Error>())
}

/// Returns the code of `err`: the outermost workflow failure in its chain, else the outermost configuration error, else [`WORKFLOW_FAILED`].
pub fn error_code<'a>(err: &'a (dyn StdError + 'static)) -> &'a str {
    if let Some(err) = engine_errors(err).find(|e| matches!(e, Error::Workflow { .. })) {
        match err {
            Error::Workflow { code, .. } => code.as_str(),
            _ => unreachable!(),
        }
    } else if let Some(err) = engine_errors(err).find(|e| matches!(e, Error::Config { .. })) {
        match err {
            Error::Config { code, .. } => code.as_str(),
            _ => unreachable!(),
        }
    } else {
        WORKFLOW_FAILED
    }
}

/// Returns the pohunek error code of the outermost pohunek CLI failure in the chain of `err`, or `None` when the CLI reported no code.
pub fn pohunek_error_code<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a str> {
    engine_errors(err)
        .find(|e| matches!(e, Error::PohunekCode { .. }))
        .map(|err| match err {
            Error::PohunekCode { code, .. } => code.as_str(),
            _ => unreachable!(),
        })
}

#[cfg(test)]
mod tests {
    use super::{BoxError, Error, WORKFLOW_FAILED, error_code, pohunek_error_code};

    #[test]
    fn error_code_reads_the_innermost_workflow_error() {
        #[derive(Debug, thiserror::Error)]
        #[error("{context}: {source}")]
        struct Wrapped {
            context: &'static str,
            source: BoxError,
        }

        let inner = Error::workflow("inner_code", false, None);
        let wrapped_twice = Wrapped {
            context: "outer",
            source: Box::new(Wrapped {
                context: "inner",
                source: Box::new(inner),
            }),
        };
        assert_eq!(error_code(&wrapped_twice), "inner_code");

        // A workflow failure inside another workflow failure is not the innermost one:
        // the first match from the top of the chain is returned.
        let outer = Error::workflow(
            "outer_code",
            false,
            Some(Box::new(Error::workflow("inner_code", false, None))),
        );
        assert_eq!(error_code(&outer), "outer_code");
        assert_eq!(outer.to_string(), "outer_code: inner_code");

        let wrapped_config = Error::workflow(
            "workflow_code",
            true,
            Some(Box::new(Error::config(
                "configuration_invalid",
                "KEY",
                None,
            ))),
        );
        assert_eq!(error_code(&wrapped_config), "workflow_code");

        let config_over_workflow = Error::config(
            "configuration_invalid",
            "KEY",
            Some(Box::new(Error::workflow("inner_code", false, None))),
        );
        assert_eq!(error_code(&config_over_workflow), "inner_code");

        let uncoded = std::io::Error::other("boom");
        assert_eq!(error_code(&uncoded), WORKFLOW_FAILED);

        let pohunek = Error::pohunek_code(
            "runtime_supervision_ambiguous",
            Box::new(Error::workflow("pohunek_request_failed", false, None)),
        );
        assert_eq!(error_code(&pohunek), "pohunek_request_failed");
        assert_eq!(
            pohunek_error_code(&pohunek),
            Some("runtime_supervision_ambiguous")
        );
        assert_eq!(pohunek_error_code(&uncoded), None);
    }
}
