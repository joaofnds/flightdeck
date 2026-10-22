//! Writes the keys this Mac's SMC lists, and the info and the bytes of each key the collector
//! asks about, as the file the SMC tests read, so those tests run on a machine without an SMC.
//! Run it on an M5 Pro to record the tests' fixture again:
//!
//! ```sh
//! cargo run -p collector --bin smc_fixture -- --keys collector/fixtures/smc/m5_pro_keys.bin
//! ```

use std::process::ExitCode;

fn main() -> ExitCode {
    let path = match collector::arguments("smc_fixture").and_then(|arguments| path_from(&arguments))
    {
        Ok(path) => path,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };

    let fixture = match collector::smc::fixture() {
        Ok(fixture) => fixture,
        Err(error) => {
            eprintln!("smc_fixture: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = std::fs::write(&path, fixture) {
        eprintln!("smc_fixture: writing {path}: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn path_from(arguments: &[String]) -> Result<String, String> {
    match arguments {
        [flag, path] if flag == "--keys" => Ok(path.clone()),
        _ => Err("usage: smc_fixture --keys PATH".to_owned()),
    }
}
