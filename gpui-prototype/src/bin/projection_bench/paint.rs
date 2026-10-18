use std::ffi::OsString;
use std::hint::black_box;
use std::io;
use std::path::Path;
use std::time::Instant;

use gpui::{Bounds, Pixels, point, px, size};
use serde_json::{Value, json};

use crate::charts::{Geometry, Inspection, prepare_lines};
use crate::data::Panel;
use crate::projection_bench::Counter;

struct Case {
    name: &'static str,
    panels: Vec<Panel>,
    hidden: Vec<String>,
    inspection: Inspection,
}

pub fn run(arguments: &[OsString]) -> Result<(), Box<dyn std::error::Error>> {
    if arguments.len() != 6 {
        return Err("--paint-input PANEL_BITS --iterations 1..100 --output NEW_FILE".into());
    }

    if arguments[2] != "--iterations" {
        return Err("Expected --iterations".into());
    }

    if arguments[4] != "--output" {
        return Err("Expected --output".into());
    }

    let iterations: u32 = arguments[3]
        .to_str()
        .ok_or("Invalid iteration text")?
        .parse()?;
    if !(1..=100).contains(&iterations) {
        return Err("Iterations must be 1..100".into());
    }

    let panels = decode(&crate::read_value(Path::new(&arguments[1]))?)?;
    let cases = cases(panels)?;
    let mut output = Vec::with_capacity(cases.len());
    for case in &cases {
        output.push(measure(case, iterations)?);
    }
    crate::write(Path::new(&arguments[5]), &json!(output))?;

    Ok(())
}

fn decode(value: &Value) -> Result<Vec<Panel>, Box<dyn std::error::Error>> {
    let mut values = value
        .as_array()
        .ok_or("Panel bits require an array")?
        .clone();
    if !(1..=7).contains(&values.len()) {
        return Err("Panel count must be 1..7".into());
    }

    for panel in &mut values {
        for key in ["minimum", "maximum", "fill"] {
            decode_number(&mut panel[key])?;
        }
        decode_numbers(&mut panel["positions"])?;
        let lines = panel["lines"].as_array_mut().ok_or("Missing lines")?;
        if lines.len() > 128 {
            return Err("Line count exceeds 128".into());
        }

        for line in lines {
            decode_number(&mut line["fill"])?;
            decode_numbers(&mut line["values"])?;
        }
    }
    let panels: Vec<Panel> = serde_json::from_value(json!(values))?;
    for panel in &panels {
        if panel.kind != "line" {
            continue;
        }

        if panel.positions.as_ref().map(Vec::len) != Some(900) {
            return Err("Paint comparison requires 900 positions per panel".into());
        }

        if panel.segments.as_ref().map(Vec::len) != Some(900) {
            return Err("Paint comparison requires 900 segments per panel".into());
        }

        for line in &panel.lines {
            if line.values.len() != 900 {
                return Err("Paint comparison requires 900 values per line".into());
            }
        }
    }

    Ok(panels)
}

fn decode_number(value: &mut Value) -> Result<(), Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(());
    }

    let bits = u32::try_from(value.as_u64().ok_or("Invalid floating-point bits")?)?;
    let number = f32::from_bits(bits);
    if !number.is_finite() {
        return Err("Nonfinite floating-point bits".into());
    }

    *value = json!(number);

    Ok(())
}

fn decode_numbers(value: &mut Value) -> Result<(), Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(());
    }

    let values = value.as_array_mut().ok_or("Missing floating-point array")?;
    if values.len() > crate::data::LIVE_SAMPLE_LIMIT {
        return Err("Floating-point array exceeds 4096 points".into());
    }

    for value in values {
        decode_number(value)?;
    }

    Ok(())
}

fn cases(panels: Vec<Panel>) -> Result<Vec<Case>, Box<dyn std::error::Error>> {
    let panels: Vec<_> = panels
        .into_iter()
        .filter(|panel| panel.kind == "line")
        .collect();
    let mut combined = panels.first().ok_or("No line panels")?.clone();
    combined.lines = panels
        .iter()
        .flat_map(|panel| panel.lines.clone())
        .collect();
    if !(8..=128).contains(&combined.lines.len()) {
        return Err("Memory fixture requires 8..128 line series".into());
    }
    let inspection = Inspection {
        lower: 0,
        selected: 899,
        upper: 899,
    };
    let hidden = combined
        .lines
        .iter()
        .map(|line| line.name.clone())
        .collect();
    let mut result = vec![
        Case {
            name: "memory",
            panels,
            hidden: Vec::new(),
            inspection,
        },
        Case {
            name: "combined-memory",
            panels: vec![combined.clone()],
            hidden: Vec::new(),
            inspection,
        },
        Case {
            name: "all-hidden",
            panels: vec![combined.clone()],
            hidden,
            inspection,
        },
        Case {
            name: "single-point",
            panels: vec![combined.clone()],
            hidden: Vec::new(),
            inspection: Inspection {
                lower: 449,
                selected: 449,
                upper: 449,
            },
        },
    ];
    result.push(varied_case(combined));

    Ok(result)
}

fn varied_case(mut combined: Panel) -> Case {
    combined.stack = Some(true);
    combined.lines[0].stack = Some(false);
    combined.lines[1].negative = Some(true);
    combined.lines[2].axis = Some("right".into());
    combined.lines[3].dashed = Some(true);
    combined.lines[4].values.truncate(800);
    for line in &mut combined.lines {
        for (index, value) in line.values.iter_mut().enumerate() {
            if index % 23 == 0 {
                *value = None;
            }
        }
    }
    combined.segments = Some((0..900).map(|index| u64::from(index >= 450)).collect());
    let hidden = vec![combined.lines[5].name.clone()];
    Case {
        name: "stacked-negative-gaps-segments-hidden",
        panels: vec![combined],
        hidden,
        inspection: Inspection {
            lower: 17,
            selected: 777,
            upper: 882,
        },
    }
}

fn measure(case: &Case, iterations: u32) -> io::Result<Value> {
    let expected = transcript(case);
    let mut measurements =
        Vec::with_capacity(usize::try_from(iterations).map_err(io::Error::other)?);
    for iteration in 0..iterations {
        let started = Instant::now();
        let counter = Counter::start();
        for panel in &case.panels {
            prepare_lines(
                panel,
                bounds(),
                case.inspection,
                &case.hidden,
                |line, values, geometry| {
                    black_box((line, values, geometry));
                },
            );
        }
        let counts = counter.finish();
        let elapsed_ns = started.elapsed().as_nanos();
        if transcript(case) != expected {
            return Err(io::Error::other(
                "Paint preparation output changed between iterations",
            ));
        }
        measurements.push(json!({"iteration": iteration, "allocation_calls": counts.0,
            "requested_bytes": counts.1, "elapsed_ns": elapsed_ns}));
    }
    println!(
        "{}",
        json!({"mode":"paint-preparation", "case":case.name,
        "panels":case.panels.len(), "series":case.panels.iter().map(|panel| panel.lines.len())
            .sum::<usize>(), "source_points":900, "measurements":measurements,
        "counter":"Rust GlobalAlloc requests during production prepare_lines only",
        "scope":"Prepared float bits/geometry/nulls/segments; excludes paths, GPU and pixels"})
    );

    Ok(json!({"case":case.name, "prepared":expected}))
}

fn bounds() -> Bounds<Pixels> {
    Bounds::new(point(px(40.0), px(8.0)), size(px(520.0), px(162.0)))
}

fn transcript(case: &Case) -> Value {
    let mut panels = Vec::with_capacity(case.panels.len());
    for panel in &case.panels {
        let mut lines = Vec::with_capacity(panel.lines.len());
        prepare_lines(
            panel,
            bounds(),
            case.inspection,
            &case.hidden,
            |line, values, geometry| {
                lines.push(
                    json!({"name":line.name, "stack":line.stack, "negative":line.negative,
                "dashed":line.dashed, "axis":line.axis, "color":line.color,
                "values":values.iter().map(|value| value.map(|(top,bottom)|
                    [top.to_bits(),bottom.to_bits()])).collect::<Vec<_>>(),
                "geometry":geometry_bits(values, geometry)}),
                );
            },
        );
        panels.push(json!({"id":panel.id, "lines":lines}));
    }

    json!(panels)
}

fn geometry_bits(values: &[Option<(f32, f32)>], geometry: &Geometry<'_>) -> Value {
    let vertices: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value.map(|(top, bottom)| {
                let top = geometry.point(index, top);
                let bottom = geometry.point(index, bottom);
                [
                    f32::from(top.x).to_bits(),
                    f32::from(top.y).to_bits(),
                    f32::from(bottom.x).to_bits(),
                    f32::from(bottom.y).to_bits(),
                ]
            })
        })
        .collect();
    json!({"minimum":geometry.minimum.to_bits(), "maximum":geometry.maximum.to_bits(),
        "count":geometry.count, "lower":geometry.lower,
        "positions":geometry.positions.map(|values| values.iter().copied()
            .map(f32::to_bits).collect::<Vec<_>>()), "segments":geometry.segments,
        "bounds":[f32::from(geometry.bounds.origin.x).to_bits(),
            f32::from(geometry.bounds.origin.y).to_bits(),
            f32::from(geometry.bounds.size.width).to_bits(),
            f32::from(geometry.bounds.size.height).to_bits()], "vertices":vertices})
}
