#[path = "../diagnostics.rs"]
mod diagnostics;

use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("text-backend-check: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().skip(1).take(2).collect();
    if arguments.as_slice() != ["--check"] {
        return Err("Use --check".into());
    }
    diagnostics::initialize()?;
    let application = diagnostics::application();
    diagnostics::verify_text_backend(&application.text_system())
}
