//! Command-line parsing for `ch-runner`.

use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "usage: ch-runner --version
       ch-runner serve --workdir <path> [--omp <program>]
       ch-runner preflight [--workdir <path>] [--omp <program>] [--test-model]";

const DEFAULT_OMP: &str = "omp";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Version,
    Serve {
        workdir: PathBuf,
        omp: OsString,
    },
    /// `test_model` reads a model test request from stdin and spends a little on the key.
    /// Without `workdir` only omp is checked.
    Preflight {
        workdir: Option<PathBuf>,
        omp: OsString,
        test_model: bool,
    },
}

/// Parses the arguments after the program name.
pub fn parse<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err("missing a command".to_owned());
    };
    match first.to_str() {
        Some("--version" | "-V") => match args.next() {
            None => Ok(Command::Version),
            Some(extra) => Err(format!("unexpected argument {}", extra.display())),
        },
        Some("serve") => {
            let (workdir, omp, _) = parse_flags(args, false)?;
            let workdir = workdir.ok_or("serve needs --workdir")?;
            Ok(Command::Serve { workdir, omp })
        }
        Some("preflight") => {
            let (workdir, omp, test_model) = parse_flags(args, true)?;
            Ok(Command::Preflight {
                workdir,
                omp,
                test_model,
            })
        }
        _ => Err(format!("unknown command {}", first.display())),
    }
}

fn parse_flags(
    mut args: impl Iterator<Item = OsString>,
    takes_test_model: bool,
) -> Result<(Option<PathBuf>, OsString, bool), String> {
    let mut workdir: Option<OsString> = None;
    let mut omp: Option<OsString> = None;
    let mut test_model = false;
    while let Some(flag) = args.next() {
        let (name, slot) = match flag.to_str() {
            Some("--test-model") if takes_test_model && !test_model => {
                test_model = true;
                continue;
            }
            Some("--test-model") if takes_test_model => {
                return Err("--test-model given twice".to_owned());
            }
            Some(name @ "--workdir") => (name, &mut workdir),
            Some(name @ "--omp") => (name, &mut omp),
            _ => return Err(format!("unknown argument {}", flag.display())),
        };
        if slot.is_some() {
            return Err(format!("{name} given twice"));
        }
        let value = args.next().filter(|value| !value.is_empty());
        *slot = Some(value.ok_or_else(|| format!("{name} needs a value"))?);
    }
    Ok((
        workdir.map(PathBuf::from),
        omp.unwrap_or_else(|| DEFAULT_OMP.into()),
        test_model,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn reads_version() {
        assert_eq!(parse_strs(&["--version"]), Ok(Command::Version));
        assert_eq!(parse_strs(&["-V"]), Ok(Command::Version));
        assert!(parse_strs(&["--version", "x"]).is_err());
    }

    #[test]
    fn reads_serve_with_default_omp() {
        assert_eq!(
            parse_strs(&["serve", "--workdir", "wt/-s1"]),
            Ok(Command::Serve {
                workdir: "wt/-s1".into(),
                omp: "omp".into()
            })
        );
    }

    #[test]
    fn reads_serve_with_omp_in_any_order() {
        assert_eq!(
            parse_strs(&["serve", "--omp", "/opt/omp", "--workdir", "/w"]),
            Ok(Command::Serve {
                workdir: "/w".into(),
                omp: "/opt/omp".into()
            })
        );
    }

    #[test]
    fn refuses_bad_serve_arguments() {
        for args in [
            &["serve"][..],
            &["serve", "--workdir"],
            &["serve", "--workdir", ""],
            &["serve", "--workdir", "a", "--workdir", "b"],
            &["serve", "--workdir", "a", "--omp"],
            &["serve", "--workdir", "a", "--verbose"],
            &["serve", "--workdir", "a", "extra"],
        ] {
            assert!(parse_strs(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn reads_preflight() {
        assert_eq!(
            parse_strs(&["preflight", "--workdir", "/w"]),
            Ok(Command::Preflight {
                workdir: Some("/w".into()),
                omp: "omp".into(),
                test_model: false,
            })
        );
        assert_eq!(
            parse_strs(&["preflight", "--test-model", "--omp", "o", "--workdir", "/w"]),
            Ok(Command::Preflight {
                workdir: Some("/w".into()),
                omp: "o".into(),
                test_model: true,
            })
        );
    }

    #[test]
    fn preflight_without_a_workdir_checks_only_omp() {
        assert_eq!(
            parse_strs(&["preflight", "--test-model"]),
            Ok(Command::Preflight {
                workdir: None,
                omp: "omp".into(),
                test_model: true,
            })
        );
    }

    #[test]
    fn refuses_bad_preflight_arguments() {
        for args in [
            &["preflight", "--workdir"][..],
            &["preflight", "--workdir", ""],
            &[
                "preflight",
                "--workdir",
                "/w",
                "--test-model",
                "--test-model",
            ],
            &["preflight", "--workdir", "/w", "--workdir", "/x"],
            &["serve", "--workdir", "/w", "--test-model"],
        ] {
            assert!(parse_strs(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn refuses_unknown_commands() {
        assert!(parse_strs(&[]).is_err());
        // The app reads this wording, exit code 2 and `unknown command preflight`, to tell a
        // runner that predates a command from one that failed. Keep it for new commands too.
        assert_eq!(parse_strs(&["run"]), Err("unknown command run".to_owned()));
    }
}
