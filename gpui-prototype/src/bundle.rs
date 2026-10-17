pub fn create() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let output = executable
        .parent()
        .ok_or("executable has no directory")?
        .parent()
        .ok_or("profile has no directory")?
        .join("Flightdeck GPUI.app");
    let contents = output.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;
    std::fs::create_dir_all(contents.join("Helpers"))?;
    std::fs::copy(&executable, contents.join("MacOS/flightdeck-gpui"))?;
    std::fs::write(contents.join("Info.plist"), include_bytes!("../Info.plist"))?;
    std::fs::write(
        contents.join("Resources/schematic_chassis.png"),
        include_bytes!("../resources/schematic_chassis.png"),
    )?;
    std::fs::write(
        contents.join("Resources/schematic_rotor.png"),
        include_bytes!("../resources/schematic_rotor.png"),
    )?;
    let collector =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/release/collector");
    if collector.is_file() {
        std::fs::copy(&collector, contents.join("Helpers/collector"))?;
        let persister = collector
            .parent()
            .ok_or("Collector has no parent")?
            .join("persister");
        std::fs::copy(persister, contents.join("Helpers/persister"))?;
    }
    println!("{}", output.display());
    Ok(())
}
