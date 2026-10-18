//! Opt-in retained compatibility source preparation; normal sealing belongs to the persister.
#[path = "compatibility_fixture/interrupted.rs"]
mod interrupted;
#[path = "compatibility_fixture/inventory.rs"]
mod inventory;
#[path = "compatibility_fixture/source.rs"]
mod source;

use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use persistence::resources::Watchdog;
use persistence::{Error, Result, Store};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("compatibility_fixture: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Legacy,
    Identity,
    Mixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceLayout {
    Legacy,
    Identity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interruption {
    PendingSynced,
    ReadyRenamed,
}

struct Options {
    root: PathBuf,
    layout: Layout,
    interruption: Option<Interruption>,
}

fn run() -> Result<()> {
    persistence::engine::initialize_process()?;
    let mut arguments = Vec::with_capacity(7);
    for argument in std::env::args_os().skip(1).take(7) {
        let argument = argument
            .into_string()
            .map_err(|_| Error::Bound("fixture arguments must be UTF-8"))?;
        if argument.len() > 1024 {
            return Err(Error::Bound("fixture argument bytes"));
        }

        arguments.push(argument);
    }

    let mut parsed = None;
    parse(&arguments, &mut parsed)?;
    let options = parsed
        .as_ref()
        .ok_or(Error::Bound("fixture options absent"))?;
    let watchdog = Watchdog::with_operation_limit(2 * 1024 * 1024 * 1024, Duration::from_secs(30))?;
    let operation = watchdog.operation()?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&options.root)?;
    let store = Store::open(&options.root)?;
    let layouts: &[SourceLayout] = match options.layout {
        Layout::Legacy => &[SourceLayout::Legacy],
        Layout::Identity => &[SourceLayout::Identity],
        Layout::Mixed => &[SourceLayout::Legacy, SourceLayout::Identity],
    };
    let mut sources = Vec::with_capacity(2);
    for (index, layout) in layouts.iter().enumerate() {
        let mut source = None;
        source::create(
            &options.root,
            store.recording(),
            *layout,
            u64::try_from(index).map_err(|_| Error::Bound("fixture run index"))?,
            &mut source,
        )?;
        sources.push(source.ok_or(Error::Bound("fixture source absent"))?);
    }

    if let Some(boundary) = options.interruption {
        interrupted::prepare(&store, &sources[0], boundary)?;
    }
    drop(store);
    let result = inventory::print(options, &sources);
    drop(operation);
    drop(watchdog);

    result
}

fn parse(arguments: &[String], output: &mut Option<Options>) -> Result<()> {
    assert!(output.is_none());
    if !matches!(arguments.len(), 4 | 6) {
        return Err(Error::Bound(
            "--recording PATH --layout legacy|identity|mixed [--interrupt MODE]",
        ));
    }

    if arguments[0] != "--recording" {
        return Err(Error::Bound("expected --recording PATH"));
    }

    let root = PathBuf::from(&arguments[1]);
    if !root.is_absolute() {
        return Err(Error::Bound("fixture root must be absolute"));
    }

    if arguments[1].len() > 1024 {
        return Err(Error::Bound("fixture root bytes"));
    }

    if arguments[2] != "--layout" {
        return Err(Error::Bound("expected --layout MODE"));
    }

    let layout = match arguments[3].as_str() {
        "legacy" => Layout::Legacy,
        "identity" => Layout::Identity,
        "mixed" => Layout::Mixed,
        _ => return Err(Error::Bound("unknown fixture layout")),
    };
    let interruption = parse_interruption(arguments, layout)?;

    *output = Some(Options {
        root,
        layout,
        interruption,
    });

    Ok(())
}

fn parse_interruption(arguments: &[String], layout: Layout) -> Result<Option<Interruption>> {
    if arguments.len() == 4 {
        return Ok(None);
    }

    if arguments[4] != "--interrupt" {
        return Err(Error::Bound("expected --interrupt MODE"));
    }

    if layout != Layout::Identity {
        return Err(Error::Bound("interruption requires identity layout"));
    }

    match arguments[5].as_str() {
        "pending-synced" => Ok(Some(Interruption::PendingSynced)),
        "ready-renamed" => Ok(Some(Interruption::ReadyRenamed)),
        _ => Err(Error::Bound("unknown fixture interruption")),
    }
}

#[cfg(test)]
mod tests {
    use super::{Interruption, Layout};

    #[test]
    fn refuses_interruptions_for_ordinary_legacy_sources() {
        // Goal: only actual identity staging can form an interrupted fixture.
        // Method: parse a legacy request carrying an identity-only interruption boundary.
        let arguments = [
            "--recording",
            "/private/tmp/compatibility-fixture-parser",
            "--layout",
            "legacy",
            "--interrupt",
            "pending-synced",
        ]
        .map(str::to_owned);

        let mut options = None;

        let result = super::parse(&arguments, &mut options);

        assert!(result.is_err());
    }

    #[test]
    fn accepts_the_ready_identity_interruption() {
        // Goal: the exact supported ready boundary remains addressable by the opt-in CLI.
        // Method: parse the public flag shape and compare its requested operation.
        let arguments = [
            "--recording",
            "/private/tmp/compatibility-fixture-parser",
            "--layout",
            "identity",
            "--interrupt",
            "ready-renamed",
        ]
        .map(str::to_owned);

        let mut options = None;

        super::parse(&arguments, &mut options).unwrap();

        let options = options.unwrap();

        assert_eq!(Layout::Identity, options.layout);
        assert_eq!(Some(Interruption::ReadyRenamed), options.interruption);
    }

    #[test]
    fn refuses_unknown_layouts() {
        // Goal: an unsupported layout cannot silently create a legacy fixture.
        // Method: pass an unknown layout through the same CLI parser.
        let arguments = [
            "--recording",
            "/private/tmp/compatibility-fixture-parser",
            "--layout",
            "unknown",
        ]
        .map(str::to_owned);

        let mut options = None;

        assert!(super::parse(&arguments, &mut options).is_err());
    }
}
