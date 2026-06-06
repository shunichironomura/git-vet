#![expect(
    clippy::print_stderr,
    reason = "Existing CLI error reporting uses eprintln until output handling is refactored"
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match git_vet::run_cli() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("git-vet: {error}");
            ExitCode::from(2)
        }
    }
}
