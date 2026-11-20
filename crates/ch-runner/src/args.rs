//! Command-line parsing for `ch-runner`.

use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str =
    "usage: ch-runner --version\n       ch-runner serve --workdir <path> [--omp <program>]";

const DEFAULT_OMP: &str = "omp";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Version,
    Serve { workdir: PathBuf, omp: OsString },
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
        Some("serve") => parse_serve(args),
        _ => Err(format!("unknown command {}", first.display())),
    }
}

fn parse_serve(mut args: impl Iterator<Item = OsString>) -> Result<Command, String> {
    let mut workdir: Option<OsString> = None;
    let mut omp: Option<OsString> = None;
    while let Some(flag) = args.next() {
        let (name, slot) = match flag.to_str() {
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
    let workdir = workdir.ok_or("serve needs --workdir")?;
    Ok(Command::Serve {
        workdir: PathBuf::from(workdir),
        omp: omp.unwrap_or_else(|| DEFAULT_OMP.into()),
    })
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
    fn refuses_unknown_commands() {
        assert!(parse_strs(&[]).is_err());
        assert!(parse_strs(&["run"]).is_err());
    }
}
