use std::time::Duration;

use gpui::{App, Context, Entity, Global, WeakEntity};

use super::lifecycle::CollectorSession;
use crate::live_dashboard::LiveDashboard;
use crate::live_timer::Timer;

#[cfg(test)]
#[path = "collector_lifecycle_application_tests.rs"]
mod tests;

/// How often the app asks the worker for its answer while one is owed, so a page opens within a
/// frame of the worker finishing its read.
const QUICK_POLL_INTERVAL: Duration = Duration::from_millis(8);

struct Collection {
    session: CollectorSession,
    timer: Option<Timer>,
    /// Runs only while the worker owes an answer, and is suspended the rest of the time.
    quick_timer: Option<Timer>,
}

impl Global for Collection {}

pub(super) fn install(session: CollectorSession, cx: &mut App) {
    cx.set_global(Collection {
        session,
        timer: None,
        quick_timer: None,
    });
    cx.on_app_quit(|cx| {
        let collection = cx.global_mut::<Collection>();
        if let Some(timer) = collection.timer.take() {
            timer.set_running(false);
        }

        // The collector saves its last readings and exits on its own once asked, and also when
        // the app that started it is gone, so quitting asks it and does not wait for it.
        collection.session.begin_stop();
        if let Some(failure) = collection.session.failure() {
            eprintln!("Flightdeck collector shutdown: {failure}");
        }
        std::future::ready(())
    })
    .detach();
}

pub(super) fn start_timer(entity: &Entity<LiveDashboard>, cx: &mut App) -> Result<(), String> {
    let page = entity.downgrade();
    let application = cx.to_async();
    let quick_page = page.clone();
    let quick_application = application.clone();
    let timer = Timer::start(move || {
        application.update(|cx| tick(&page, cx));
    })?;
    let quick_timer = Timer::with_interval(QUICK_POLL_INTERVAL, move || {
        quick_application.update(|cx| quick_tick(&quick_page, cx));
    })?;
    quick_timer.set_running(false);
    let collection = cx.global_mut::<Collection>();
    collection.timer = Some(timer);
    collection.quick_timer = Some(quick_timer);
    // Every change that sends the worker a request notifies the page, so watching the page here
    // takes each answer within a frame, whichever change sent its request.
    cx.observe(entity, |dashboard, cx| {
        if dashboard.read(cx).awaiting_reply() {
            poll_quickly(cx);
        }
    })
    .detach();
    Ok(())
}

/// Starts polling the worker every few milliseconds, until it has answered the last request.
fn poll_quickly(cx: &App) {
    if let Some(collection) = cx.try_global::<Collection>() {
        if let Some(timer) = &collection.quick_timer {
            timer.set_running(true);
        }
    }
}

fn quick_tick(entity: &WeakEntity<LiveDashboard>, cx: &mut App) {
    let settled = entity
        .update(cx, |dashboard, cx| {
            dashboard.tick(cx);
            !dashboard.awaiting_reply()
        })
        .unwrap_or(true);
    if settled {
        if let Some(timer) = &cx.global::<Collection>().quick_timer {
            timer.set_running(false);
        }
    }
}

fn tick(entity: &WeakEntity<LiveDashboard>, cx: &mut App) {
    if entity.update(cx, dashboard_tick).is_err() {
        cx.global_mut::<Collection>().session.tick();
    }
}

fn dashboard_tick(dashboard: &mut LiveDashboard, cx: &mut Context<LiveDashboard>) {
    dashboard.tick(cx);
    super::preferences::remember(dashboard, cx);
    // A request sent with no quick poll, such as the page's first at launch, is still taken
    // within a frame of its answer from the next tick on.
    if dashboard.awaiting_reply() {
        poll_quickly(cx);
    }
    let collection = cx.global_mut::<Collection>();
    let previous = collection.session.status();
    collection.session.tick();
    let mut changed = previous != collection.session.status();
    let reports = collection.session.reports();
    if reports.revision != dashboard.self_reports.revision {
        dashboard.self_reports = std::rc::Rc::new(reports.clone());
        if dashboard.request.route == "self" {
            changed = true;
        }
    }
    if changed {
        cx.notify();
    }
}

pub fn collector_status(cx: &App) -> Option<(String, bool)> {
    cx.try_global::<Collection>()
        .and_then(|collection| collection.session.status())
}

/// Stops the owned collector and reaps it, which quitting leaves to the collector itself.
#[cfg(test)]
pub(super) fn stop_and_wait(cx: &mut App) -> Result<(), String> {
    cx.global_mut::<Collection>().session.stop_and_wait()
}

pub fn retry_collector(cx: &mut App) {
    if cx.try_global::<Collection>().is_some() {
        cx.global_mut::<Collection>().session.retry();
        cx.refresh_windows();
    }
}
