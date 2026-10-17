mod bundle;
mod chart_view;
mod charts;
mod dashboard;
mod data;
mod hardware;
mod live_dashboard;
mod live_executables;
mod live_hardware;
mod live_inventory;
mod live_launch;
mod live_notices;
mod live_probe;
mod live_projection;
mod live_source;
mod live_tables;
mod live_timer;
mod live_views;
mod performance;
mod summary;

#[cfg(test)]
mod tests;

use std::process::ExitCode;

use gpui::{App, AppContext as _, Application, Bounds, WindowBounds, WindowOptions, px, size};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flightdeck-gpui: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--bundle") {
        return bundle::create();
    }
    let options = performance::Options::parse()?;
    if options.source.check {
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
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1320.0), px(900.0)), cx);
        let options_window = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Flightdeck GPUI prototype".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        match cx.open_window(options_window, move |_, cx| {
            cx.new(|_| {
                dashboard::Dashboard::new(capture, catalog, performance::Performance::new(options))
            })
        }) {
            Ok(_) => cx.activate(true),
            Err(error) => {
                eprintln!("window failed: {error}");
                cx.quit();
            }
        }
    });
    Ok(())
}
