use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

pub fn create() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let arguments: Vec<_> = std::env::args().skip(1).take(3).collect();

    if arguments == ["--bundle", "--check"] {
        validate_bundle(&executable)?;
        println!("Validated packaged Flightdeck and both recording helpers");
        return Ok(());
    }

    if arguments != ["--bundle"] {
        return Err("Use --bundle or --bundle --check".into());
    }

    let output = executable
        .parent()
        .ok_or("Executable has no directory")?
        .parent()
        .ok_or("Profile has no directory")?
        .join("Flightdeck.app");
    let collector = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/release/collector");
    let persister = collector
        .parent()
        .ok_or("Collector has no parent")?
        .join("persister");

    let inputs = Inputs {
        executable,
        collector,
        persister,
        output,
    };

    create_from(&inputs)?;
    println!("{}", inputs.output.display());

    Ok(())
}

struct Inputs {
    executable: PathBuf,
    collector: PathBuf,
    persister: PathBuf,
    output: PathBuf,
}

const RESOURCES: [(&str, &[u8]); 3] = [
    ("Info.plist", include_bytes!("../Info.plist")),
    (
        "Resources/schematic_chassis.png",
        include_bytes!("../resources/schematic_chassis.png"),
    ),
    (
        "Resources/schematic_rotor.png",
        include_bytes!("../resources/schematic_rotor.png"),
    ),
];

fn create_from(inputs: &Inputs) -> io::Result<()> {
    validate_name(&inputs.executable)?;
    validate_executable(&inputs.executable)?;
    validate_executable(&inputs.collector)?;
    validate_executable(&inputs.persister)?;

    let contents = inputs.output.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;
    std::fs::create_dir_all(contents.join("Helpers"))?;
    std::fs::copy(&inputs.executable, contents.join("MacOS/flightdeck"))?;
    std::fs::copy(&inputs.collector, contents.join("Helpers/collector"))?;
    std::fs::copy(&inputs.persister, contents.join("Helpers/persister"))?;
    for (name, bytes) in RESOURCES {
        std::fs::write(contents.join(name), bytes)?;
    }
    validate_bundle(&contents.join("MacOS/flightdeck"))
}

fn validate_bundle(executable: &Path) -> io::Result<()> {
    validate_name(executable)?;
    validate_executable(executable)?;
    let directory = executable.parent().ok_or(io::ErrorKind::InvalidInput)?;
    if directory.file_name() != Some(std::ffi::OsStr::new("MacOS")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Bundle validation must run from Contents/MacOS/flightdeck",
        ));
    }

    let contents = directory.parent().ok_or(io::ErrorKind::InvalidInput)?;
    if contents.file_name() != Some(std::ffi::OsStr::new("Contents")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Missing bundle Contents",
        ));
    }

    validate_executable(&contents.join("Helpers/collector"))?;
    validate_executable(&contents.join("Helpers/persister"))?;

    for (name, expected) in RESOURCES {
        let path = contents.join(name);
        if std::fs::metadata(&path)?.len()
            != u64::try_from(expected.len()).map_err(io::Error::other)?
        {
            return Err(resource_error(&path));
        }

        if std::fs::read(&path)? != expected {
            return Err(resource_error(&path));
        }
    }

    Ok(())
}

fn validate_name(path: &Path) -> io::Result<()> {
    if path.file_name() != Some(std::ffi::OsStr::new("flightdeck")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Bundle input must be the flightdeck application executable",
        ));
    }

    Ok(())
}

fn validate_executable(path: &Path) -> io::Result<()> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Required executable is not a regular file: {}",
                path.display()
            ),
        ));
    }

    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("Required file is not executable: {}", path.display()),
        ));
    }

    Ok(())
}

fn resource_error(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "Packaged resource differs from this build: {}",
            path.display()
        ),
    )
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
