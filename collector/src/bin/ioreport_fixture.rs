//! Writes the `IOReport` channels of this Mac's groups that a read takes from, and one sample
//! of those a read keeps, as the binary property lists the `IOReport` tests read, so those
//! tests run on a machine without the channels. Run it on an M5 Pro to record the tests'
//! fixtures again:
//!
//! ```sh
//! cargo run -p collector --bin ioreport_fixture -- \
//!   --groups collector/fixtures/ioreport/m5_pro_groups.plist \
//!   --sample collector/fixtures/ioreport/m5_pro_sample.plist
//! ```

use std::process::ExitCode;

fn main() -> ExitCode {
    let paths = match collector::arguments("ioreport_fixture")
        .and_then(|arguments| paths_from(&arguments))
    {
        Ok(paths) => paths,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };

    // The fixtures are written only once both are read whole, so a failed read leaves the
    // recorded ones as they were.
    let mut fixture = collector::ioreport::Fixture {
        groups: Vec::new(),
        sample: Vec::new(),
    };
    if let Err(error) = collector::ioreport::fixture(&mut fixture) {
        eprintln!("ioreport_fixture: {error}");
        return ExitCode::FAILURE;
    }

    for (path, bytes) in [
        (&paths.groups, &fixture.groups),
        (&paths.sample, &fixture.sample),
    ] {
        if let Err(error) = std::fs::write(path, bytes) {
            eprintln!("ioreport_fixture: writing {path}: {error}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

struct Paths {
    groups: String,
    sample: String,
}

fn paths_from(arguments: &[String]) -> Result<Paths, String> {
    match arguments {
        [groups_flag, groups, sample_flag, sample]
            if groups_flag == "--groups" && sample_flag == "--sample" =>
        {
            Ok(Paths {
                groups: groups.clone(),
                sample: sample.clone(),
            })
        }
        _ => Err("usage: ioreport_fixture --groups PATH --sample PATH".to_owned()),
    }
}
