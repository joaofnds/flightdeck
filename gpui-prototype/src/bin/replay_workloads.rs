use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("replay_workloads: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if ![2, 4].contains(&arguments.len()) {
        return Err("replay_workloads --output NEW_DIRECTORY [--seconds 30..600]".into());
    }
    if arguments[0] != "--output" {
        return Err("Expected --output NEW_DIRECTORY".into());
    }
    let output = Path::new(&arguments[1]);
    let seconds = duration(&arguments)?;
    let duration_ms = u64::from(seconds) * 1000;
    fs::create_dir(output)?;
    let initial = json!({"at_ms":0,"route":"overview","sample":120,"minutes":15});
    write(output, "idle-overview", &json!([initial]), duration_ms)?;
    for route in ["overview", "cpu"] {
        let actions: Vec<_> = (0..seconds * 10)
            .map(|index| {
                json!({"at_ms":u64::from(index)*100,"route":route,
                "sample":90+index%31,"minutes":15})
            })
            .collect();
        write(
            output,
            &format!("inspection-{route}"),
            &json!(actions),
            duration_ms,
        )?;
    }
    let hardware = json!([{"at_ms":0,"route":"overview","sample":120,
        "minutes":15,"scroll":"hardware"}]);
    write(output, "hardware", &hardware, duration_ms)?;
    println!("{}", output.display());
    Ok(())
}

fn duration(arguments: &[std::ffi::OsString]) -> Result<u16, Box<dyn std::error::Error>> {
    if arguments.len() == 2 {
        return Ok(30);
    }
    if arguments[2] != "--seconds" {
        return Err("Expected --seconds".into());
    }
    let seconds = arguments[3].to_str().ok_or("Invalid duration")?.parse()?;
    if !(30..=600).contains(&seconds) {
        return Err("Duration must be 30..600 seconds".into());
    }
    Ok(seconds)
}

fn write(output: &Path, name: &str, actions: &Value, duration_ms: u64) -> io::Result<()> {
    let plan = json!({"schema":1,"duration_ms":duration_ms,"actions":actions});
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join(format!("{name}.json")))?;
    let bytes = serde_json::to_vec_pretty(&plan).map_err(io::Error::other)?;
    file.write_all(&bytes)
}
