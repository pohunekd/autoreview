//! Command-line front end of the changelog contract. Reads `changelog.toml`
//! from the current directory, which the `scripts/changelog` wrapper sets to
//! the repository root.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use pohunek_companion_changelog::{CONFIG_FILE, Config, Error, check, fold, notes, today_utc};

const USAGE: &str = "usage:
  changelog check
  changelog fold <version> [--surface <name>] [--date YYYY-MM-DD]
  changelog notes <version> [--surface <name>]";

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
                let mut options = options.iter();
                while let Some(option) = options.next() {
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
                    Ok(Self::Notes { version, surface })
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
            Self::Notes { version, surface } => {
                print!("{}", notes(root, &config, &version, surface.as_deref())?);
            }
        }
        Ok(())
    }
}
