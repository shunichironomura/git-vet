use std::io::{self, Write};
use std::process::ExitCode;

use console::Style;

fn main() -> ExitCode {
    match git_vet::run_cli() {
        Ok(code) => code,
        Err(error) => {
            let label = Style::new().red().bold().for_stderr().apply_to("error:");
            let _ = writeln!(io::stderr().lock(), "git-vet {label} {error}");
            ExitCode::from(2)
        }
    }
}
