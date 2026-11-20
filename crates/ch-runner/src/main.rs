use std::io;
use std::process::ExitCode;

use ch_runner::args::{self, Command, USAGE};
use ch_runner::{CurlPrOpener, Options, serve};

fn main() -> ExitCode {
    match args::parse(std::env::args_os().skip(1)) {
        Ok(Command::Version) => {
            println!("ch-runner {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Serve { workdir, omp }) => {
            let options = Options {
                workdir,
                omp_program: omp,
                pr_opener: Box::new(CurlPrOpener),
            };
            match serve(io::stdin(), io::stdout(), options) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("ch-runner: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("ch-runner: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
