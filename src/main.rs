use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match git_vet::run_cli() {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "git-vet: {error}");
            ExitCode::from(2)
        }
    }
}
