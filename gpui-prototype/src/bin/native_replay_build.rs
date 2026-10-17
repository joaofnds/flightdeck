#[path = "../benchmark_helpers.rs"]
pub mod benchmark_helpers;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use benchmark_helpers::{OwnedChild, hash, logs, read_log, write_json};
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("native_replay_build: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let prototype = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = prototype.parent().ok_or("Missing repository root")?;
    let options = options(prototype)?;
    let output = std::env::current_dir()?.join(options.output);
    let source_archive = root.join("target/release/libviewer.a");
    if !options.typecheck {
        if !source_archive.is_file() {
            return Err(
                "Build target/release/libviewer.a before the native replay baseline".into(),
            );
        }
    }
    fs::create_dir_all(output.parent().ok_or("Missing output parent")?)?;
    fs::create_dir(&output)?;
    let source_revision = export(root, &output)?;
    patch(&output)?;
    let helper = prototype.join("benchmark/native_replay.swift");
    fs::copy(&helper, output.join("desktop/sources/native_replay.swift"))?;
    if options.typecheck {
        typecheck(&output)?;
        write_json(
            &output.join("typecheck.json"),
            &json!({"schema":1,
            "desktop_source_revision":source_revision,"replay_helper_sha256":hash(&helper)?,
            "mode":"Swift captured views in AppKit replay shell; typecheck only",
            "compiler":"xcrun swiftc -typecheck -swift-version 6 -warnings-as-errors",
            "viewer_archive_required":false}),
        )?;
        return Ok(output);
    }
    let archive = output.join("libviewer.a");
    fs::copy(&source_archive, &archive)?;
    let app = output.join("Flightdeck Swift Replay.app");
    fs::create_dir_all(app.join("Contents/MacOS"))?;
    fs::create_dir_all(app.join("Contents/Resources"))?;
    fs::write(app.join("Contents/Info.plist"), INFO)?;
    let executable = app.join("Contents/MacOS/FlightdeckSwiftReplay");
    compile(&output, &archive, &executable)?;
    let resources = resources(&output, &app)?;
    write_json(
        &output.join("build.json"),
        &json!({"schema":1,
        "desktop_source_revision":source_revision,"viewer_archive":archive,
        "viewer_archive_source":source_archive,
        "viewer_archive_sha256":hash(&archive)?,"replay_helper_sha256":hash(&helper)?,
        "executable":executable,"executable_sha256":hash(&executable)?,
        "resources":resources,"compiler":"xcrun swiftc -swift-version 6 -warnings-as-errors -O",
        "scope":"Swift captured views in AppKit replay shell",
        "application_shell":concat!("Explicit NSApplication/NSWindow/NSHostingView; ",
            "archived @main excluded"),
        "limitations":["Viewer archive hash identifies its bytes, not its source revision",
            "Captured ReferenceConsole views retained; original SwiftUI WindowGroup not exercised",
            "No collector helpers packaged; window construction is not presentation evidence"]}),
    )?;
    Ok(executable)
}

struct Options {
    output: PathBuf,
    typecheck: bool,
}

fn options(prototype: &Path) -> Result<Options, Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() > 3 {
        return Err("native_replay_build [--typecheck] [--output NEW_DIRECTORY]".into());
    }
    let mut options = Options {
        output: prototype.join("target/native-replay-source"),
        typecheck: false,
    };
    let mut arguments = arguments.iter();
    for _attempt in 0..3 {
        let Some(flag) = arguments.next() else { break };
        match flag.to_str().ok_or("Invalid native replay flag")? {
            "--typecheck" => options.typecheck = true,
            "--output" => {
                options.output = PathBuf::from(arguments.next().ok_or("--output needs a path")?);
            }
            flag => return Err(format!("Unknown native replay flag {flag}").into()),
        }
    }
    Ok(options)
}

fn export(root: &Path, output: &Path) -> io::Result<String> {
    let revision_logs = logs(output, "source-revision");
    let mut revision = Command::new("git");
    revision.current_dir(root).args(["rev-parse", "HEAD"]);
    checked(&mut revision, &revision_logs, Duration::from_secs(30))?;
    let revision = read_log(&revision_logs.stdout)?.trim().to_owned();
    if revision.len() != 40 {
        return Err(io::Error::other("Unexpected source revision length"));
    }
    if !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("Invalid source revision"));
    }
    let mut archive = Command::new("git");
    archive
        .current_dir(root)
        .args(["archive", "--format=tar", "--output"])
        .arg(output.join("desktop.tar"))
        .arg(&revision)
        .arg("desktop");
    checked(
        &mut archive,
        &logs(output, "archive"),
        Duration::from_secs(30),
    )?;
    let mut extract = Command::new("/usr/bin/tar");
    extract
        .arg("-xf")
        .arg(output.join("desktop.tar"))
        .arg("-C")
        .arg(output);
    checked(
        &mut extract,
        &logs(output, "extract"),
        Duration::from_secs(30),
    )?;
    Ok(revision)
}

fn checked(
    command: &mut Command,
    logs: &benchmark_helpers::Logs,
    budget: Duration,
) -> io::Result<()> {
    let mut child = OwnedChild::start(command, logs)?;
    let status = child.wait(budget)?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "Build command failed: {status}; see {}",
            logs.stderr.display()
        )));
    }
    Ok(())
}

fn replace(path: &Path, before: &str, after: &str) -> io::Result<()> {
    let source = fs::read_to_string(path)?;
    if source.match_indices(before).take(2).count() != 1 {
        return Err(io::Error::other(format!(
            "Pinned Swift patch anchor changed in {}",
            path.display()
        )));
    }
    fs::write(path, source.replacen(before, after, 1))
}

fn patch(output: &Path) -> io::Result<()> {
    let console = output.join("desktop/sources/reference_console.swift");
    replace(
        &console,
        "                ScrollView {\n                    if let error = model.error {",
        concat!(
            "                ScrollViewReader { replayScroll in\n                ScrollView {\n",
            "                    Color.clear.frame(height: 0).id(\"native-replay-top\")\n",
            "                    if let error = model.error {"
        ),
    )?;
    replace(
        &console,
        "                }\n                HStack {\n                    Text(model.route",
        concat!(
            "                }\n",
            "                .coordinateSpace(name: \"native-replay-viewport\")\n",
            "                .onGeometryChange(for: CGSize.self) { $0.size } action: { size in\n",
            "                    NativeReplay.hardwareViewport(size)\n                }\n",
            "                .onReceive(NotificationCenter.default.publisher(\n",
            "                    for: NativeReplay.scrollNotification)) { notification in\n",
            "                    guard let target = notification.userInfo?[\"scroll\"] ",
            "as? String\n",
            "                    else { return }\n",
            "                    DispatchQueue.main.async {\n",
            "                        let destination = target == \"top\"\n",
            "                            ? \"native-replay-top\" : \"native-replay-hardware\"\n",
            "                        NativeReplay.scrollDispatched(destination)\n",
            "                        replayScroll.scrollTo(destination, anchor: .top)\n",
            "                    }\n                }\n                }\n",
            "                HStack {\n                    Text(model.route"
        ),
    )?;
    patch_hardware_panel(output)?;
    patch_hardware_geometry(output)
}

fn patch_hardware_panel(output: &Path) -> io::Result<()> {
    let pages = output.join("desktop/sources/reference_pages.swift");
    replace(
        &pages,
        "                    ReferenceSection(group: group)",
        concat!(
            "                    ReferenceSection(group: group)\n",
            "                        .id(group.panels.contains(where: { ",
            "$0.kind == \"hardware\" })\n",
            "                            ? \"native-replay-hardware\" : group.id)"
        ),
    )?;
    replace(
        &pages,
        concat!(
            "case \"hardware\": MacBookSchematic(readings: schematicReadings, ",
            "preview: model.schematicPreview)"
        ),
        concat!(
            "case \"hardware\": MacBookSchematic(readings: schematicReadings,\n",
            "            preview: model.schematicPreview)\n",
            "            .onGeometryChange(for: CGRect.self) { geometry in\n",
            "                geometry.frame(in: .named(\"native-replay-viewport\"))\n",
            "            } action: { NativeReplay.hardwarePanelGeometry($0) }"
        ),
    )
}

fn patch_hardware_geometry(output: &Path) -> io::Result<()> {
    let schematic = output.join("desktop/sources/schematic.swift");
    replace(
        &schematic,
        "        .onChange(of: animates) { updateMotion() }",
        concat!(
            "        .onChange(of: animates) { updateMotion() }\n",
            "        .onGeometryChange(for: CGRect.self) { geometry in\n",
            "            geometry.frame(in: .named(\"native-replay-viewport\"))\n",
            "        } action: { NativeReplay.hardwareGeometry($0) }\n",
            "        .onChange(of: animates && running, initial: true) { _, enabled in\n",
            "            NativeReplay.hardwareAnimation(enabled)\n        }\n",
            "        .onDisappear { NativeReplay.hardwareGone() }"
        ),
    )
}

fn sources(output: &Path) -> io::Result<Vec<PathBuf>> {
    let mut sources = Vec::with_capacity(64);
    for entry in fs::read_dir(output.join("desktop/sources"))?.take(65) {
        let entry = entry?;
        if entry.file_name() == "flightdeck.swift" {
            continue;
        }
        if entry.path().extension() == Some(std::ffi::OsStr::new("swift")) {
            sources.push(entry.path());
        }
    }
    if sources.len() > 64 {
        return Err(io::Error::other("Swift source count exceeds 64"));
    }
    sources.sort();
    Ok(sources)
}

fn compiler(output: &Path) -> Command {
    let mut compiler = Command::new("xcrun");
    compiler
        .args(["swiftc", "-swift-version", "6", "-warnings-as-errors", "-I"])
        .arg(output.join("desktop/bridge"));
    compiler
}

fn typecheck(output: &Path) -> io::Result<()> {
    let mut compiler = compiler(output);
    compiler.arg("-typecheck").args(sources(output)?);
    checked(
        &mut compiler,
        &logs(output, "typecheck"),
        Duration::from_mins(5),
    )
}

fn compile(output: &Path, archive: &Path, executable: &Path) -> io::Result<()> {
    let mut compiler = compiler(output);
    compiler
        .arg("-O")
        .args([
            "-lc++",
            "-lIOReport",
            "-framework",
            "IOKit",
            "-framework",
            "CoreFoundation",
            "-framework",
            "Security",
        ])
        .arg(archive)
        .args(sources(output)?)
        .arg("-o")
        .arg(executable);
    checked(
        &mut compiler,
        &logs(output, "compile"),
        Duration::from_mins(5),
    )
}

fn resources(output: &Path, app: &Path) -> io::Result<Vec<serde_json::Value>> {
    let mut resources = Vec::with_capacity(64);
    let mut total = 0_u64;
    for entry in fs::read_dir(output.join("desktop/resources"))?.take(65) {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| io::Error::other("Resource size"))?;
        if total > 32 * 1024 * 1024 {
            return Err(io::Error::other("Resource size exceeds 32 MiB"));
        }
        if !metadata.is_file() {
            return Err(io::Error::other("Expected a regular resource"));
        }
        let path = app.join("Contents/Resources").join(entry.file_name());
        fs::copy(entry.path(), &path)?;
        resources.push(json!({"name":entry.file_name().to_string_lossy(),"sha256":hash(&path)?}));
    }
    if resources.len() > 64 {
        return Err(io::Error::other("Resource count exceeds 64"));
    }
    Ok(resources)
}

const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
"http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>FlightdeckSwiftReplay</string>
<key>CFBundleIdentifier</key><string>dev.flightdeck.swift-replay</string>
<key>CFBundleName</key><string>Flightdeck Swift Replay</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>"#;
