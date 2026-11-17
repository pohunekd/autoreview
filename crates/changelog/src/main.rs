//! Command-line front end of the changelog contract. Reads `changelog.toml`
//! from the current directory, which the `scripts/changelog` wrapper sets to
//! the repository root.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use pohunek_companion_changelog::{
    CONFIG_FILE, Config, END_MARKER, Error, START_MARKER, check, fold, notes, today_utc,
};

const USAGE: &str = "usage:
  changelog check
  changelog fold <version> [--surface <name>] [--date YYYY-MM-DD]
  changelog notes <version> [--surface <name>] [--post]";

/// Token carried by every part of a release post that a person still has to write.
const POST_PLACEHOLDER: &str = "TODO(release-post)";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = match Command::parse(&args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("changelog: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match command.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("changelog: {error}");
            ExitCode::FAILURE
        }
    }
}

enum Command {
    Check,
    Fold {
        version: String,
        date: Option<String>,
        surface: Option<String>,
    },
    Notes {
        version: String,
        surface: Option<String>,
        post: bool,
    },
}

impl Command {
    fn parse(args: &[String]) -> Result<Self, String> {
        let (name, rest) = args.split_first().ok_or("missing command")?;
        match name.as_str() {
            "check" if rest.is_empty() => Ok(Self::Check),
            "fold" | "notes" => {
                let (version, options) = rest
                    .split_first()
                    .ok_or_else(|| format!("{name} needs a version"))?;
                let mut surface = None;
                let mut date = None;
                let mut post = false;
                let mut options = options.iter();
                while let Some(option) = options.next() {
                    if name == "notes" && option == "--post" {
                        post = true;
                        continue;
                    }
                    let value = options
                        .next()
                        .ok_or_else(|| format!("{option} needs a value"))?
                        .clone();
                    match (name.as_str(), option.as_str()) {
                        (_, "--surface") => surface = Some(value),
                        ("fold", "--date") => date = Some(value),
                        _ => return Err(format!("unknown option for {name}: {option}")),
                    }
                }
                let version = version.clone();
                if name == "fold" {
                    Ok(Self::Fold {
                        version,
                        date,
                        surface,
                    })
                } else {
                    Ok(Self::Notes {
                        version,
                        surface,
                        post,
                    })
                }
            }
            _ => Err(format!("unknown command: {name}")),
        }
    }

    fn run(self) -> Result<(), Error> {
        let root = Path::new(".");
        let config = Config::load(Path::new(CONFIG_FILE))?;
        match self {
            Self::Check => {
                let count = check(root, &config)?;
                println!("changelog: {count} fragment(s) valid");
            }
            Self::Fold {
                version,
                date,
                surface,
            } => {
                let date = match date {
                    Some(date) => date,
                    None => today_utc()?,
                };
                fold(root, &config, &version, &date, surface.as_deref())?;
                println!("changelog: folded {version} ({date})");
            }
            Self::Notes {
                version,
                surface,
                post,
            } => {
                let block = notes(root, &config, &version, surface.as_deref())?;
                print!("{}", render_notes(block, post)?);
            }
        }
        Ok(())
    }
}

/// Returns the release notes block as is, or as a release post skeleton when `post` is set.
fn render_notes(block: String, post: bool) -> Result<String, Error> {
    if post {
        post_skeleton(&block)
    } else {
        Ok(block)
    }
}

/// Wraps a release notes block into a release post skeleton for a person to write.
///
/// The folded section moves under `### Full changelog` with its kind headings one level down.
fn post_skeleton(block: &str) -> Result<String, Error> {
    let section = block
        .strip_prefix(&format!("{START_MARKER}\n"))
        .and_then(|rest| rest.strip_suffix(&format!("\n{END_MARKER}\n")))
        .ok_or_else(|| Error::Changelog("release notes block lacks its markers".to_owned()))?;
    let full_changelog: Vec<String> = section
        .lines()
        .map(|line| {
            if line.starts_with("### ") {
                format!("#{line}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    let full_changelog = full_changelog.join("\n");
    Ok(format!(
        "{START_MARKER}\n\
         {POST_PLACEHOLDER}: two or three sentences on what this release is about.\n\
         \n\
         ### What's new\n\
         \n\
         **{POST_PLACEHOLDER}: highlight.** What a user can now do and why it matters, \
         in plain words.\n\
         \n\
         ### Upgrading\n\
         \n\
         {POST_PLACEHOLDER}: what an operator does when upgrading, or that nothing is needed.\n\
         \n\
         ### Full changelog\n\
         \n\
         {full_changelog}\n\
         {END_MARKER}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = concat!(
        "<!-- pohunek-changelog:start -->\n",
        "### Added\n\n- A ([#1](u))\n\n### Fixed\n\n- B ([#2](u))\n",
        "<!-- pohunek-changelog:end -->\n"
    );

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn post_skeleton_keeps_the_markers_once_and_moves_the_headings_down() {
        let post = render_notes(BLOCK.to_owned(), true).unwrap();
        assert_eq!(post.matches(START_MARKER).count(), 1);
        assert_eq!(post.matches(END_MARKER).count(), 1);
        assert!(post.starts_with(&format!("{START_MARKER}\n{POST_PLACEHOLDER}")));
        assert!(post.ends_with(&format!("\n{END_MARKER}\n")));
        assert_eq!(post.matches(POST_PLACEHOLDER).count(), 3);
        let headings: Vec<&str> = post.lines().filter(|l| l.starts_with('#')).collect();
        assert_eq!(
            headings,
            [
                "### What's new",
                "### Upgrading",
                "### Full changelog",
                "#### Added",
                "#### Fixed"
            ]
        );
        assert!(post.contains("- A ([#1](u))\n\n#### Fixed\n\n- B ([#2](u))\n<!--"));
    }

    #[test]
    fn plain_notes_stay_unchanged_and_a_block_without_markers_is_refused() {
        assert_eq!(render_notes(BLOCK.to_owned(), false).unwrap(), BLOCK);
        assert!(render_notes("### Added\n".to_owned(), true).is_err());
    }

    #[test]
    fn post_is_an_option_of_notes_only() {
        let parsed = Command::parse(&args(&["notes", "0.6.0", "--post"]));
        assert!(matches!(parsed, Ok(Command::Notes { post: true, .. })));
        let parsed = Command::parse(&args(&["notes", "0.6.0"]));
        assert!(matches!(parsed, Ok(Command::Notes { post: false, .. })));
        assert!(Command::parse(&args(&["fold", "0.6.0", "--post"])).is_err());
    }
}
