#[path = "projection_bench/app.rs"]
mod app;
#[path = "../chart_axes.rs"]
pub mod chart_axes;
#[path = "../charts.rs"]
pub mod charts;
#[path = "../panel_data.rs"]
pub mod data;
#[path = "../live_inventory.rs"]
pub mod live_inventory;
#[path = "../live_projection.rs"]
pub mod live_projection;
#[cfg(test)]
#[path = "../live_store_fixtures.rs"]
mod live_store_fixtures;
#[path = "projection_bench/paint.rs"]
mod paint;
#[path = "projection_bench/mod.rs"]
mod projection_bench;
#[path = "../render_evidence.rs"]
pub mod render_evidence;

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("projection_bench: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Selection {
    value: Value,
    scalar_points: u32,
    expected: Value,
}

impl Selection {
    fn parse(value: Value, route: &str) -> io::Result<Self> {
        let points = value["trends"]
            .as_array()
            .ok_or_else(|| io::Error::other("Selection requires a trends array"))?;
        if !(1..=data::LIVE_SAMPLE_LIMIT).contains(&points.len()) {
            return Err(io::Error::other("Selection requires 1..4096 trend points"));
        }

        if !matches!(value.get("selected"), Some(Value::Null | Value::Object(_))) {
            return Err(io::Error::other(
                "Selection requires selected identity or null",
            ));
        }

        for point in points {
            if !point.is_object() {
                return Err(io::Error::other("Selection requires object trend points"));
            }

            let publication = &point["publication"];
            if !unsigned_publication(publication) {
                return Err(io::Error::other(
                    "Trend point requires unsigned publication",
                ));
            }
        }

        let scalar_points = u32::try_from(points.len()).map_err(io::Error::other)?;
        let panels = live_projection::panels(route, &value, None).map_err(io::Error::other)?;
        let expected = projection_bench::bits(&panels);

        Ok(Self {
            value,
            scalar_points,
            expected,
        })
    }
}

fn unsigned_publication(value: &Value) -> bool {
    if value.as_u64().is_some() {
        return true;
    }

    let Some(text) = value.as_str() else {
        return false;
    };
    if text.len() > 20 {
        return false;
    }

    text.parse::<u64>().is_ok()
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    persistence::engine::initialize_process()?;
    let arguments: Vec<_> = std::env::args_os().skip(1).take(13).collect();
    if matches!(
        arguments.first().and_then(|value| value.to_str()),
        Some("--capture-app" | "--app-input")
    ) {
        return app::run(&arguments);
    }

    if arguments
        .first()
        .is_some_and(|value| value == "--paint-input")
    {
        return paint::run(&arguments);
    }
    if arguments.len() != 8 {
        if arguments.len() == 4 {
            if arguments[0] == "--capture" {
                if arguments[2] == "--output" {
                    return capture(Path::new(&arguments[1]), Path::new(&arguments[3]));
                }
            }
        }

        return Err("--input FILE --route ROUTE --iterations 1..100 --output NEW_FILE".into());
    }

    if arguments[0] != "--input" {
        return Err("Expected --input".into());
    }

    if arguments[2] != "--route" {
        return Err("Expected --route".into());
    }

    if arguments[4] != "--iterations" {
        return Err("Expected --iterations".into());
    }

    if arguments[6] != "--output" {
        return Err("Expected --output".into());
    }

    let route = arguments[3].to_str().ok_or("Route is not Unicode")?;
    if !["overview", "cpu", "memory", "gpu", "energy", "network"].contains(&route) {
        return Err("Unsupported scalar route".into());
    }

    let iterations = arguments[5]
        .to_str()
        .ok_or("Invalid iteration text")?
        .parse()?;
    if !(1..=100).contains(&iterations) {
        return Err("Iterations must be 1..100".into());
    }

    measure(
        &read(Path::new(&arguments[1]), route)?,
        route,
        iterations,
        Path::new(&arguments[7]),
    )
}

fn capture(recording: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = viewer::Session::open(recording)?;
    let second = session.catalog()["last_second"]
        .as_u64()
        .ok_or("Recording has no saved second")?;
    let selection = session.select(second, 0)?;
    let count = selection["trends"]
        .as_array()
        .ok_or("Missing trends")?
        .len();
    if u64::try_from(count)? != viewer::CHARTED_SECONDS {
        return Err("Capture requires a real scalar point every charted second".into());
    }

    write(output, &selection)?;
    println!("{}", json!({"mode": "capture", "scalar_points": count}));

    Ok(())
}

fn measure(
    selection: &Selection,
    route: &str,
    iterations: u32,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut measurements = Vec::with_capacity(usize::try_from(iterations)?);
    for iteration in 0..iterations {
        let started = Instant::now();
        let counter = projection_bench::Counter::start();
        let panels = live_projection::panels(route, &selection.value, None);
        let counts = counter.finish();
        let elapsed_ns = started.elapsed().as_nanos();
        let panels = panels?;
        if projection_bench::bits(&panels) != selection.expected {
            return Err("Projection output changed between iterations".into());
        }

        measurements.push(json!({"iteration": iteration, "allocation_calls": counts.0,
            "requested_bytes": counts.1, "elapsed_ns": elapsed_ns}));
    }

    write(output, &selection.expected)?;
    println!(
        "{}",
        json!({"mode": "projection", "route": route,
        "scalar_points": selection.scalar_points,
        "counter": "Rust GlobalAlloc requests during panels only; not live allocator bytes",
        "measurements": measurements})
    );

    Ok(())
}

fn read(path: &Path, route: &str) -> io::Result<Selection> {
    Selection::parse(read_value(path)?, route)
}

fn read_value(path: &Path) -> io::Result<Value> {
    const BYTES_MAX: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();

    File::open(path)?
        .take(BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(io::Error::other)? > BYTES_MAX {
        return Err(io::Error::other("Projection input exceeds 16 MiB"));
    }

    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn write(path: &Path, value: &Value) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
    file.write_all(b"\n")
}
