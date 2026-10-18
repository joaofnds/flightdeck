//! Opt-in macOS application build and launch, without a local hook or toolchain installer.
#[path = "flightdeck/command.rs"]
mod command;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flightdeck: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let command = arguments.first().map_or("--help", String::as_str);
    if command == "--help" {
        println!(
            "flightdeck --build | --check | --run [--recording PATH]\n\
            Builds the native app in target/Flightdeck.app using installed Rust and Xcode tools."
        );
        return Ok(());
    }
    if !matches!(command, "--build" | "--check" | "--run") {
        return Err(io::Error::other("expected --build, --check or --run"));
    }
    if arguments.len() > 1 {
        if arguments.len() != 3 {
            return Err(io::Error::other("expected --recording PATH"));
        }
        if arguments[1] != "--recording" {
            return Err(io::Error::other("expected --recording PATH"));
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace path"))?;
    let archive = archive(root)?;
    let app = build(root, &archive)?;
    if command == "--check" {
        check(root, &archive)?;
    }
    println!("{}", app.display());
    if command == "--run" {
        let mut launch = Command::new(app.join("Contents/MacOS/Flightdeck"));
        launch.args(&arguments[1..]);
        success(launch.status()?)?;
    }
    Ok(())
}

fn archive(root: &Path) -> io::Result<PathBuf> {
    let output = cargo(root)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "viewer",
            "--lib",
            "--message-format=json-render-diagnostics",
        ])
        .output()?;
    io::stderr().write_all(&output.stderr)?;
    success(output.status)?;
    if output.stdout.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("cargo output bound"));
    }
    let mut archive = None;
    for line in output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: serde_json::Value = serde_json::from_slice(line).map_err(io::Error::other)?;
        if message["reason"] == "compiler-artifact" {
            if message["target"]["name"] == "viewer" {
                if let Some(files) = message["filenames"].as_array() {
                    for file in files {
                        if let Some(path) = file.as_str() {
                            if Path::new(path).extension() == Some(std::ffi::OsStr::new("a")) {
                                archive = Some(PathBuf::from(path));
                            }
                        }
                    }
                }
            }
        }
    }
    archive.ok_or_else(|| io::Error::other("cargo returned no viewer archive"))
}

fn cargo(root: &Path) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(root);
    // Cargo-run metadata otherwise changes ring's build fingerprint in nested invocations.
    for name in [
        "CARGO_MANIFEST_DIR",
        "CARGO_PKG_NAME",
        "CARGO_PKG_VERSION_MAJOR",
        "CARGO_PKG_VERSION_MINOR",
        "CARGO_PKG_VERSION_PATCH",
        "CARGO_PKG_VERSION_PRE",
    ] {
        command.env_remove(name);
    }

    command
}

fn compiler(root: &Path, archive: &Path) -> Command {
    let mut compile = Command::new("xcrun");
    compile
        .current_dir(root)
        .args([
            "swiftc",
            "-swift-version",
            "6",
            "-warnings-as-errors",
            "-O",
            "-I",
            "desktop/bridge",
            "-lc++",
            "-lIOReport",
            "-framework",
            "IOKit",
            "-framework",
            "CoreFoundation",
            "-framework",
            "Security",
        ])
        .arg(archive);
    compile
}

fn check(root: &Path, archive: &Path) -> io::Result<()> {
    let executable = root.join("target/native-state-checks");
    let sources = sources(root)?;
    success(
        compiler(root, archive)
            .args(
                sources.iter().filter(|path| {
                    path.file_name() != Some(std::ffi::OsStr::new("flightdeck.swift"))
                }),
            )
            .args([
                "desktop/tests/recording_state.swift",
                "desktop/tests/reference_data_checks.swift",
                "desktop/tests/dashboard_checks.swift",
                "desktop/tests/computer_inventory_checks.swift",
                "desktop/tests/selected_cpu_checks.swift",
                "desktop/tests/cpu_history_checks.swift",
                "desktop/tests/cpu_history_panel_checks.swift",
                "desktop/tests/memory_pressure_checks.swift",
                "desktop/tests/memory_swap_checks.swift",
                "desktop/tests/memory_quantities_checks.swift",
                "desktop/tests/battery_checks.swift",
                "desktop/tests/storage_checks.swift",
                "desktop/tests/fan_history_checks.swift",
                "desktop/tests/live_collector_checks.swift",
                "desktop/tests/self_monitoring_checks.swift",
                "desktop/tests/live_apps_checks.swift",
                "desktop/tests/live_apps_worker_checks.swift",
            ])
            .arg("-o")
            .arg(&executable)
            .status()?,
    )?;
    success(Command::new(&executable).status()?)?;
    success(
        cargo(root)
            .args([
                "run",
                "--locked",
                "-p",
                "viewer",
                "--bin",
                "native_fixture",
                "--",
                "--checks",
            ])
            .arg(&executable)
            .status()?,
    )
}

fn build(root: &Path, archive: &Path) -> io::Result<PathBuf> {
    let app = root.join("target/Flightdeck.app");
    let executable = app.join("Contents/MacOS");
    fs::create_dir_all(&executable)?;
    fs::write(app.join("Contents/Info.plist"), INFO)?;
    resources(root, &app)?;
    helpers(&HelperPaths { root, app: &app })?;
    let mut compile = compiler(root, archive);
    compile
        .args(sources(root)?)
        .arg("-o")
        .arg(executable.join("Flightdeck"));
    success(compile.status()?)?;
    success(
        Command::new("codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(&app)
            .status()?,
    )?;
    Ok(app)
}

struct HelperPaths<'a> {
    root: &'a Path,
    app: &'a Path,
}

fn helpers(paths: &HelperPaths<'_>) -> io::Result<()> {
    let deadline = command::Deadline::build();
    let directory = paths.app.join("Contents/Helpers");

    fs::create_dir_all(&directory)?;

    for (package, name) in [("collector", "collector"), ("persistence", "persister")] {
        let output = command::run(
            cargo(paths.root).args([
                "build",
                "--locked",
                "--release",
                "-p",
                package,
                "--bin",
                name,
                "--message-format=json-render-diagnostics",
            ]),
            &deadline,
        )?;
        io::stderr().write_all(&output.stderr)?;
        success(output.status)?;

        let source = helper_executable(&output.stdout, name)?;
        let destination = directory.join(name);
        fs::copy(source, &destination)?;

        let signed = command::run(
            Command::new("codesign")
                .args(["--force", "--sign", "-", "--timestamp=none"])
                .arg(&destination),
            &deadline,
        )?;
        io::stderr().write_all(&signed.stderr)?;
        success(signed.status)?;
    }

    Ok(())
}

fn helper_executable(output: &[u8], name: &str) -> io::Result<PathBuf> {
    if output.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("cargo helper output bound"));
    }

    let mut executable = None;
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: serde_json::Value = serde_json::from_slice(line).map_err(io::Error::other)?;

        if message["reason"] == "compiler-artifact" {
            if message["target"]["name"] == name {
                if let Some(path) = message["executable"].as_str() {
                    executable = Some(PathBuf::from(path));
                }
            }
        }
    }

    executable.ok_or_else(|| io::Error::other("cargo returned no helper executable"))
}

fn sources(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut sources = Vec::new();
    for entry in fs::read_dir(root.join("desktop/sources"))? {
        if sources.len() == 64 {
            return Err(io::Error::other("Swift source count"));
        }
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "swift")
        {
            sources.push(path);
        }
    }
    sources.sort();
    Ok(sources)
}

fn resources(root: &Path, app: &Path) -> io::Result<()> {
    let directory = app.join("Contents/Resources");
    fs::create_dir_all(&directory)?;
    let mut count = 0;
    let mut bytes = 0_u64;
    for entry in fs::read_dir(root.join("desktop/resources"))? {
        count += 1;
        let entry = entry?;
        let metadata = entry.metadata()?;
        bytes += metadata.len();
        if count > 64 {
            return Err(io::Error::other("native resource count bound"));
        }
        if bytes > 32 * 1024 * 1024 {
            return Err(io::Error::other("native resource byte bound"));
        }
        if !metadata.is_file() {
            return Err(io::Error::other("native resource bound"));
        }
        fs::copy(entry.path(), directory.join(entry.file_name()))?;
    }
    Ok(())
}

fn success(status: std::process::ExitStatus) -> io::Result<()> {
    if !status.success() {
        return Err(io::Error::other(format!("command failed: {status}")));
    }
    Ok(())
}

const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
"http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>Flightdeck</string>
<key>CFBundleIdentifier</key><string>dev.flightdeck.desktop</string>
<key>CFBundleName</key><string>Flightdeck</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>"#;
