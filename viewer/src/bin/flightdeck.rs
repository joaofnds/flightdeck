//! Opt-in macOS application build and launch, without a local hook or toolchain installer.
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
    let output = Command::new("cargo")
        .current_dir(root)
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
    success(
        compiler(root, archive)
            .args([
                "desktop/sources/recording.swift",
                "desktop/sources/theme.swift",
                "desktop/sources/recorded_table.swift",
                "desktop/tests/recording_state.swift",
            ])
            .arg("-o")
            .arg(&executable)
            .status()?,
    )?;
    success(Command::new(executable).status()?)
}

fn build(root: &Path, archive: &Path) -> io::Result<PathBuf> {
    let app = root.join("target/Flightdeck.app");
    let executable = app.join("Contents/MacOS");
    fs::create_dir_all(&executable)?;
    fs::write(app.join("Contents/Info.plist"), INFO)?;
    let mut compile = compiler(root, archive);
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
    compile
        .args(sources)
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
