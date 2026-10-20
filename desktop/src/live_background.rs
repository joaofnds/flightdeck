//! The live app keeps collecting once its window closes, so no reading is missed while the
//! window is shut. The Dock icon goes with the window, and the menu bar item, or opening the
//! app again, brings the window back over everything collected meanwhile.

use std::path::PathBuf;

use gpui::{ActivationPolicy, App, Global};

use crate::live_source::Worker;
use crate::menu_bar::{Command, MenuBarItem};

#[cfg(test)]
#[path = "live_background_tests.rs"]
mod tests;

/// What a live window needs to open again after the user closed it.
pub(super) struct Reopening {
    pub root: PathBuf,
    pub page: String,
    pub hardware: bool,
}

struct Background {
    reopening: Reopening,
}

impl Global for Background {}

/// Holds the item in the menu bar for as long as the app runs.
struct MenuBar {
    _item: MenuBarItem,
}

impl Global for MenuBar {}

pub(super) fn install(reopening: Reopening, cx: &mut App) {
    cx.set_global(Background { reopening });
    cx.on_window_closed(|cx, _window_id| {
        if cx.windows().is_empty() {
            cx.set_activation_policy(ActivationPolicy::Accessory);
        }
    })
    .detach();
}

/// Puts Flightdeck in the menu bar, where a click shows the window and its menu quits.
pub(super) fn install_menu_bar(cx: &mut App) {
    let application = cx.to_async();
    let item = MenuBarItem::new(move |command| {
        application.update(|cx| match command {
            Command::Show => show(cx),
            Command::Quit => super::request_quit(cx),
        });
    });
    match item {
        Ok(item) => cx.set_global(MenuBar { _item: item }),
        Err(error) => eprintln!("Flightdeck menu bar item failed: {error}"),
    }
}

/// Brings the live window forward, and opens it again when the user closed it.
pub fn show(cx: &mut App) {
    cx.set_activation_policy(ActivationPolicy::Regular);
    if let Some(window) = cx.windows().first() {
        if let Err(error) = window.update(cx, |_, window, _| window.activate_window()) {
            eprintln!("Show Flightdeck window: {error}");
        }
    } else {
        if let Err(error) = reopen(cx) {
            eprintln!("Reopen Flightdeck window: {error}");
        }
    }
    cx.activate(true);
}

fn reopen(cx: &mut App) -> Result<(), String> {
    let reopening = &cx.global::<Background>().reopening;
    let start = super::Start {
        page: reopening.page.clone(),
        seconds: None,
        hardware: reopening.hardware,
    };
    let worker = Worker::start(reopening.root.clone()).map_err(|error| error.to_string())?;
    super::open_window(
        super::Opening {
            worker,
            start,
            control: None,
        },
        cx,
    )
}
