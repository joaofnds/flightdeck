//! Writes the keys this Mac's SMC lists, and the info and the bytes of each key the collector
//! asks about, as the file the SMC tests read, so those tests run on a machine without an SMC.
//! Run it on an M5 Pro to record the tests' fixture again:
//!
//! ```sh
//! cargo run -p collector --bin smc_fixture -- --keys collector/fixtures/smc/m5_pro_keys.bin
//! ```
//!
//! The tests in `smc::tests` hold what the recorded M5 Pro gave as literals: its 3,626 keys,
//! its two fans' speeds, its power, its hottest sensors and the key TR0Z it holds as another
//! type than a float. A new recording, even on the same Mac, fails those tests until each
//! literal is read again from it.

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

    // The fixture is written only once it is read whole, so a failed read leaves the recorded
    // one as it was.
    let mut fixture = Vec::new();
    if let Err(error) = collector::smc::fixture(&mut fixture) {
        eprintln!("smc_fixture: {error}");
        return ExitCode::FAILURE;
    }

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
