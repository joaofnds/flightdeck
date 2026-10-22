use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Inputs, create_from, validate_bundle};

struct Fixture {
    root: PathBuf,
    inputs: Inputs,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("flightdeck-bundle-{}-{serial}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let inputs = Inputs {
            executable: root.join("flightdeck"),
            collector: root.join("collector"),
            persister: root.join("persister"),
            output: root.join("Flightdeck.app"),
        };
        write_executable(&inputs.executable, b"app");
        write_executable(&inputs.collector, b"collector");
        write_executable(&inputs.persister, b"persister");
        Self { root, inputs }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn write_executable(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn complete_bundle_keeps_the_built_executables_and_resources() {
    // Goal: distribution uses these build inputs and retains executable permissions.
    // Method: assemble isolated named files and validate the resulting packaged artifact.
    let fixture = Fixture::new();

    create_from(&fixture.inputs).unwrap();
    let contents = fixture.inputs.output.join("Contents");
    validate_bundle(&contents.join("MacOS/flightdeck")).unwrap();

    assert_eq!(
        std::fs::read(contents.join("MacOS/flightdeck")).unwrap(),
        b"app"
    );
    assert_eq!(
        std::fs::read(contents.join("Helpers/collector")).unwrap(),
        b"collector"
    );
    assert_eq!(
        std::fs::read(contents.join("Helpers/persister")).unwrap(),
        b"persister"
    );
}

#[test]
fn packaged_check_rejects_a_corrupted_resource() {
    // Goal: the artifact's own validation catches an incomplete or stale resource copy.
    // Method: replace packaged artwork after assembly without changing the build inputs.
    let fixture = Fixture::new();
    create_from(&fixture.inputs).unwrap();
    let contents = fixture.inputs.output.join("Contents");
    std::fs::write(contents.join("Resources/schematic_rotor.png"), b"corrupt").unwrap();

    let result = validate_bundle(&contents.join("MacOS/flightdeck"));

    assert!(result.is_err());
}

#[test]
fn missing_collector_leaves_the_output_untouched() {
    // Goal: a missing helper cannot publish a bundle that fails after distribution.
    // Method: remove its isolated source before assembling and check the output boundary.
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.inputs.collector).unwrap();

    let result = create_from(&fixture.inputs);

    assert!(result.is_err());
    assert!(!fixture.inputs.output.exists());
}

#[test]
fn missing_persister_preserves_an_existing_bundle() {
    // Goal: failed preflight leaves the previous distributable artifact available.
    // Method: seed a prior bundle and remove the second required helper source.
    let fixture = Fixture::new();
    std::fs::create_dir(&fixture.inputs.output).unwrap();
    let previous = fixture.inputs.output.join("previous");
    std::fs::write(&previous, b"accepted bundle").unwrap();
    std::fs::remove_file(&fixture.inputs.persister).unwrap();

    let result = create_from(&fixture.inputs);

    assert!(result.is_err());
    assert_eq!(std::fs::read(previous).unwrap(), b"accepted bundle");
    assert!(!fixture.inputs.output.join("Contents").exists());
}

#[test]
fn nonexecutable_collector_cannot_be_packaged() {
    // Goal: a readable helper cannot pass packaging when the OS cannot execute it.
    // Method: remove every execution bit and inspect the output after preflight refusal.
    let fixture = Fixture::new();
    std::fs::set_permissions(
        &fixture.inputs.collector,
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    let result = create_from(&fixture.inputs);

    assert!(result.is_err());
    assert!(!fixture.inputs.output.exists());
}

#[test]
fn bundle_launch_starts_with_every_thread_default() {
    // Goal: a launch from Finder or the Dock needs no startup re-exec, which would leave
    // macOS attributing the menu bar item to the replaced program and never showing it.
    // Method: read the packaged environment and look up each default persistence fills in.
    let plist = std::str::from_utf8(super::RESOURCES[0].1).unwrap();
    let start = plist.find("<key>LSEnvironment</key><dict>").unwrap();
    let length = plist[start..].find("</dict>").unwrap();
    let environment = &plist[start..start + length];

    for (option, default) in crate::APP_THREAD_COUNTS.environment() {
        let entry = format!("<key>{option}</key><string>{default}</string>");
        assert!(environment.contains(&entry), "{entry} is missing");
    }
}

#[test]
fn rebuilt_bundle_replaces_each_executable_with_a_new_file() {
    // Goal: rebuilding never rewrites an executable in place, since macOS kills a program
    // whose file changed under the signature it read when that file last ran.
    // Method: package twice and compare the file identity and bytes of each executable.
    let fixture = Fixture::new();
    create_from(&fixture.inputs).unwrap();
    let contents = fixture.inputs.output.join("Contents");
    let names = ["MacOS/flightdeck", "Helpers/collector", "Helpers/persister"];
    let before = names.map(|name| std::fs::metadata(contents.join(name)).unwrap().ino());
    write_executable(&fixture.inputs.executable, b"rebuilt app");

    create_from(&fixture.inputs).unwrap();

    for (name, inode) in names.into_iter().zip(before) {
        assert_ne!(std::fs::metadata(contents.join(name)).unwrap().ino(), inode);
    }
    assert_eq!(
        std::fs::read(contents.join("MacOS/flightdeck")).unwrap(),
        b"rebuilt app"
    );
}
