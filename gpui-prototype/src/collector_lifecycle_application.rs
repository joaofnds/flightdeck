use gpui::{App, Context, Entity, Global, WeakEntity};

use super::lifecycle::CollectorSession;
use crate::live_dashboard::LiveDashboard;
use crate::live_timer::Timer;

#[cfg(test)]
#[path = "collector_lifecycle_application_tests.rs"]
mod tests;

struct Collection {
    session: CollectorSession,
    timer: Option<Timer>,
    quitting: bool,
    quit_dispatched: bool,
}

impl Global for Collection {}

pub(super) fn install(session: CollectorSession, cx: &mut App) {
    cx.set_global(Collection {
        session,
        timer: None,
        quitting: false,
        quit_dispatched: false,
    });
    cx.on_app_quit(|cx| {
        let collection = cx.global_mut::<Collection>();
        if !collection.quit_dispatched {
            eprintln!("Flightdeck native termination requested; saving the owned collector");
        }
        if let Some(timer) = collection.timer.take() {
            timer.set_running(false);
        }
        // GPUI invokes this observer before timing its returned future. Native termination
        // cannot be canceled here, so retain/reap ownership synchronously within our bound.
        if let Err(failure) = collection.session.stop_and_wait() {
            eprintln!("Flightdeck collector cleanup: {failure}");
        }
        std::future::ready(())
    })
    .detach();
}

pub(super) fn start_timer(entity: &Entity<LiveDashboard>, cx: &mut App) -> Result<(), String> {
    let entity = entity.downgrade();
    let application = cx.to_async();
    let timer = Timer::start(move || {
        application.update(|cx| tick(&entity, cx));
    })?;
    cx.global_mut::<Collection>().timer = Some(timer);
    Ok(())
}

fn tick(entity: &WeakEntity<LiveDashboard>, cx: &mut App) {
    if entity.update(cx, dashboard_tick).is_err() {
        cx.global_mut::<Collection>().session.tick(false);
    }
    let collection = cx.global_mut::<Collection>();
    if !collection.quitting {
        return;
    }
    if collection.quit_dispatched {
        return;
    }
    if collection.session.finished() {
        collection.quit_dispatched = true;
        if let Some(failure) = collection.session.failure() {
            eprintln!("Flightdeck collector shutdown: {failure}");
        }
        cx.quit();
    }
}

fn dashboard_tick(dashboard: &mut LiveDashboard, cx: &mut Context<LiveDashboard>) {
    dashboard.tick(cx);
    let collection = cx.global_mut::<Collection>();
    let previous = collection.session.status();
    collection
        .session
        .tick(dashboard.request.route != "computer");
    if previous != collection.session.status() {
        cx.notify();
    }
}

pub fn collector_status(cx: &App) -> Option<(String, bool)> {
    cx.try_global::<Collection>()
        .and_then(|collection| collection.session.status())
}

pub fn retry_collector(cx: &mut App) {
    if cx.try_global::<Collection>().is_some() {
        cx.global_mut::<Collection>().session.retry();
        cx.refresh_windows();
    }
}

pub fn request_quit(cx: &mut App) {
    if cx.try_global::<Collection>().is_none() {
        cx.quit();
        return;
    }
    let collection = cx.global_mut::<Collection>();
    collection.quitting = true;
    collection.session.begin_stop();
    if collection.session.finished() {
        collection.quit_dispatched = true;
        if let Some(failure) = collection.session.failure() {
            eprintln!("Flightdeck collector shutdown: {failure}");
        }
        cx.quit();
    }
}
