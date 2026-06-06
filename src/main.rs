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
