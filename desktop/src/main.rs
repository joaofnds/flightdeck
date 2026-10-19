mod bundle;
mod chart_axes;
mod chart_view;
mod charts;
#[cfg(feature = "automation")]
mod control;
mod dashboard;
mod data;
mod diagnostics;
mod hardware;
mod hardware_clock;
mod hardware_layout;
mod hardware_motion;
mod live_acceptance;
mod live_app_attribution_history;
mod live_app_views;
mod live_chart_view;
mod live_cpu;
mod live_cpu_history;
mod live_dashboard;
mod live_diagnostics;
mod live_executables;
mod live_hardware;
mod live_hover;
mod live_inventory;
mod live_launch;
mod live_memory;
mod live_notices;
mod live_overview_app_histories;
mod live_presentation;
mod live_probe;
mod live_process_history;
mod live_projection;
mod live_rankings;
mod live_resident_history;
mod live_resources;
mod live_sections;
mod live_source;
mod live_source_mode;
mod live_system;
mod live_tables;
mod live_timer;
mod live_views;
mod native_input;
mod native_text;
mod native_workflows;
mod performance;
mod render_evidence;
mod replay;
mod replay_dashboard;
mod summary;

#[cfg(test)]
mod allocation_tests;
#[cfg(test)]
#[path = "live_process_history_tests.rs"]
pub(crate) mod process_history_tests;
#[cfg(test)]
mod tests;

use std::process::ExitCode;

use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flightdeck: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    persistence::engine::initialize_process()?;
    diagnostics::initialize()?;
    if std::env::args().nth(1).as_deref() == Some("--bundle") {
        return bundle::create();
    }
    let options = performance::Options::parse()?;
    if options.source.probe != performance::ProbeMode::None {
        return live_launch::probe(&options);
    }
    if !options.source.reference {
        if !options.check() {
            return live_launch::run(options);
        }
    }
    run_reference(options)
}

fn run_reference(options: performance::Options) -> Result<(), Box<dyn std::error::Error>> {
    let replay = options
        .replay
        .as_deref()
        .map(replay::Replay::load)
        .transpose()?;
    let capture = data::Capture::load()?;
    let catalog = data::Catalog::load()?;
    if !catalog
        .navigation
        .iter()
        .any(|entry| entry[0] == options.page)
    {
        return Err(format!("unknown page: {}", options.page).into());
    }
    if options.check() {
        println!(
            "Captured prototype: {} routes, {} metric families, {} samples per hour",
            catalog.navigation.len(),
            capture.series.len(),
            data::SAMPLE_COUNT
        );
        return Ok(());
    }
    let application = diagnostics::application();
    diagnostics::verify_text_backend(&application.text_system())?;
    application.run(move |cx: &mut App| {
        run_reference_window(capture, catalog, options, replay, cx);
    });
    Ok(())
}

fn run_reference_window(
    capture: data::Capture,
    catalog: data::Catalog,
    options: performance::Options,
    replay: Option<replay::Replay>,
    cx: &mut App,
) {
    cx.on_window_closed(|cx, _window_id| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    let width = if replay.as_ref().is_some_and(replay::Replay::shows_hardware) {
        1272.0
    } else {
        1320.0
    };
    let bounds = Bounds::centered(None, size(px(width), px(900.0)), cx);
    let options_window = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("Flightdeck".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    match cx.open_window(options_window, move |_, cx| {
        let entity = gpui::AppContext::new(cx, |_| {
            dashboard::Dashboard::new(
                capture,
                catalog,
                performance::Performance::new(options),
                replay,
            )
        });
        let hardware_state = std::rc::Rc::clone(&entity.read(cx).hardware_state);
        if let Err(error) = performance::start_deadline(&entity, cx) {
            eprintln!("Reference deadline failed: {error}");
            cx.quit();
        }
        if let Err(error) = hardware_clock::install(&entity, &hardware_state, cx) {
            eprintln!("Reference hardware timer failed: {error}");
            cx.quit();
        }
        if let Err(error) = replay_dashboard::start(&entity, cx) {
            eprintln!("Reference timer failed: {error}");
            cx.quit();
        }
        entity
    }) {
        Ok(_) => cx.activate(true),
        Err(error) => {
            eprintln!("window failed: {error}");
            cx.quit();
        }
    }
}
