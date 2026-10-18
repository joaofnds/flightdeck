use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context as PollContext, Poll, Waker};
use std::thread::JoinHandle;

use gpui::{ClipboardItem, Context, Entity};
use serde_json::Value;

use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Request, Snapshot, Update, Worker};
use crate::native_input::TimeInput;

gpui::actions!(flightdeck_native, [Quit, CloseWindow]);

pub fn install_application_actions(cx: &mut gpui::App) {
    cx.bind_keys([
        gpui::KeyBinding::new("cmd-q", Quit, None),
        gpui::KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            if let Err(error) = window.update(cx, |_, window, _| window.remove_window()) {
                eprintln!("Close Flightdeck window: {error}");
            }
        }
    });
    cx.set_menus(vec![gpui::Menu {
        name: "Flightdeck".into(),
        disabled: false,
        items: vec![
            gpui::MenuItem::action("Close Window", CloseWindow),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Quit Flightdeck", Quit),
        ],
    }]);
}

type PromptPoll = Box<dyn FnMut() -> Poll<Result<Option<Vec<PathBuf>>, String>>>;

#[derive(Default)]
pub struct NativeControls {
    pub input: Option<Entity<TimeInput>>,
    prompt: Option<PromptPoll>,
    candidate: Option<Candidate>,
    retirement: Option<JoinHandle<()>>,
    pub notice: Option<String>,
}

struct Candidate {
    worker: Worker,
    request: Request,
    generation: u64,
    started: std::time::Instant,
}

impl NativeControls {
    pub fn busy(&self) -> bool {
        if self.opening() {
            return true;
        }

        self.retirement.is_some()
    }

    pub fn opening(&self) -> bool {
        if self.prompt.is_some() {
            return true;
        }

        if self.candidate.is_some() {
            return true;
        }

        false
    }

    fn retire(&mut self, worker: Worker) {
        assert!(self.retirement.is_none());
        self.retirement = worker.retire();
    }

    pub fn poll_retirement(&mut self) -> Result<bool, String> {
        if let Some(thread) = &self.retirement {
            if thread.is_finished() {
                if let Some(thread) = self.retirement.take() {
                    thread
                        .join()
                        .map_err(|_| "Reader retirement panicked".to_owned())?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

impl LiveDashboard {
    pub fn native_header_controls(&self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::dashboard::control;
        use gpui::{div, prelude::*, px};
        let mut controls = div()
            .flex()
            .gap(px(6.0))
            .items_center()
            .child(
                control("Open…")
                    .accessibility_id("flightdeck.open-recording")
                    .aria_label("Open recording folder")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_recording(cx);
                    })),
            )
            .child(
                control("Copy")
                    .accessibility_id("flightdeck.copy-reading")
                    .aria_label("Copy selected reading or expanded details")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.copy_reading(cx);
                    })),
            );
        if self.native.opening() {
            controls = controls.child(control("Cancel open").on_click(cx.listener(
                |this, _, _, cx| {
                    this.cancel_recording("Recording unchanged");
                    cx.notify();
                },
            )));
        }
        controls
    }

    pub fn native_notice(&self) -> impl gpui::IntoElement {
        use gpui::prelude::*;
        gpui::div().children(self.native.notice.as_ref().map(|notice| {
            gpui::div()
                .id("native-notice")
                .role(gpui::Role::Status)
                .accessibility_id("flightdeck.native-notice")
                .aria_label(notice.clone())
                .px(gpui::px(24.0))
                .py(gpui::px(4.0))
                .text_color(gpui::rgb(crate::charts::MUTED))
                .child(notice.clone())
        }))
    }

    pub fn open_recording(&mut self, cx: &mut Context<Self>) {
        if self.native.busy() {
            return;
        }

        let mut receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Flightdeck recording".into()),
        });

        self.native.prompt = Some(Box::new(move || {
            let mut context = PollContext::from_waker(Waker::noop());
            match Pin::new(&mut receiver).poll(&mut context) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(result)) => Poll::Ready(result.map_err(|error| error.to_string())),
                Poll::Ready(Err(error)) => Poll::Ready(Err(format!("Recording picker: {error}"))),
            }
        }));
        self.native.notice = Some("Choose a recording folder".into());

        cx.notify();
    }

    pub fn native_tick(&mut self, cx: &mut Context<Self>) {
        match self.native.poll_retirement() {
            Ok(true) => cx.notify(),
            Ok(false) => {}
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
        self.native_prompt_tick(cx);
        self.native_candidate_tick(cx);
        let submitted = self
            .native
            .input
            .as_ref()
            .and_then(|input| input.update(cx, |input, _| input.submission.take()));
        if let Some(text) = submitted {
            self.inspect_text(&text, cx);
        }
    }

    fn native_prompt_tick(&mut self, cx: &mut Context<Self>) {
        let result = self.native.prompt.as_mut().map(|prompt| prompt());
        if let Some(Poll::Ready(result)) = result {
            self.native.prompt = None;
            match result {
                Ok(Some(paths)) => {
                    if paths.len() == 1 {
                        if let Some(path) = paths.into_iter().next() {
                            self.begin_recording(path);
                        }
                    } else {
                        self.native.notice = Some("Choose exactly one recording folder".into());
                    }
                }
                Ok(None) => self.native.notice = Some("Recording unchanged".into()),
                Err(error) => self.native.notice = Some(error),
            }
            cx.notify();
        }
    }

    pub fn begin_recording(&mut self, path: PathBuf) {
        assert!(!self.native.busy());
        match Worker::start(path) {
            Ok(mut worker) => {
                let route = if self.request.route == "computer" {
                    "overview"
                } else {
                    &self.request.route
                };
                let request = fresh_request(route);
                match worker.request(request.clone()) {
                    Ok(generation) => {
                        self.native.candidate = Some(Candidate {
                            worker,
                            request,
                            generation,
                            started: std::time::Instant::now(),
                        });
                        self.native.notice =
                            Some("Opening recording; current readings retained".into());
                    }
                    Err(error) => {
                        self.native.notice = Some(error);
                        self.native.retire(worker);
                    }
                }
            }
            Err(error) => self.native.notice = Some(format!("Recording could not open: {error}")),
        }
    }

    fn native_candidate_tick(&mut self, cx: &mut Context<Self>) {
        if self.recording_step() {
            cx.notify();
        }
    }

    pub fn recording_step(&mut self) -> bool {
        let Some(candidate) = &mut self.native.candidate else {
            return false;
        };

        let result = candidate.worker.take();
        let expected = candidate.generation;
        let timed_out = candidate.started.elapsed() >= std::time::Duration::from_secs(30);

        match result {
            Ok(Some(Update::Ready(snapshot))) if snapshot.generation == expected => {
                self.accept_recording(snapshot);
                true
            }
            Ok(Some(Update::Failed {
                generation,
                message,
            })) if generation == expected => {
                self.cancel_recording(&format!("Recording unchanged: {message}"));
                true
            }
            Err(error) => {
                self.cancel_recording(&format!("Recording unchanged: {error}"));
                true
            }
            _ => {
                if timed_out {
                    self.cancel_recording("Recording unchanged: open exceeded 30 seconds");
                }
                timed_out
            }
        }
    }

    fn accept_recording(&mut self, snapshot: Box<Snapshot>) {
        if let Some(candidate) = self.native.candidate.take() {
            let old = std::mem::replace(&mut self.worker, candidate.worker);
            self.request = candidate.request;
            self.generation = candidate.generation;
            self.snapshot = None;
            self.panels.clear();
            self.expanded = None;
            self.executable = None;
            self.executables.clear();
            self.hidden.clear();
            self.hover_second = None;
            self.app_picker_open = false;
            self.accept(snapshot);
            self.native.notice = Some("Recording opened".into());
            self.native.retire(old);
        }
    }

    pub fn cancel_recording(&mut self, notice: &str) {
        self.native.prompt = None;
        if let Some(candidate) = self.native.candidate.take() {
            self.native.retire(candidate.worker);
        }
        self.native.notice = Some(notice.to_owned());
    }

    pub fn inspect_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let seconds = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.selection["trends"].as_array())
            .map_or_else(Vec::new, |points| {
                points
                    .iter()
                    .take(4096)
                    .filter_map(|point| point["wall_ns"].as_u64().map(|wall| wall / 1_000_000_000))
                    .collect()
            });
        match crate::native_text::inspection_second(text, &seconds) {
            Ok(second) => {
                self.native.notice = None;
                self.inspect(second, cx);
            }
            Err(error) => {
                self.native.notice = Some(error);
                cx.notify();
            }
        }
    }

    pub fn copy_reading(&mut self, cx: &mut Context<Self>) {
        match selected_copy(
            self.snapshot.as_deref(),
            &self.request,
            self.expanded.as_ref(),
        ) {
            Ok(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.native.notice = Some("Selected reading copied".into());
            }
            Err(error) => self.native.notice = Some(error),
        }
        cx.notify();
    }
}

fn fresh_request(route: &str) -> Request {
    Request {
        route: route.into(),
        second: None,
        identity: None,
        table: "ticks".into(),
        offset: 0,
        app: None,
        apps_offset: 0,
        members_offset: 0,
    }
}

pub fn selected_copy(
    snapshot: Option<&Snapshot>,
    request: &Request,
    expanded: Option<&(String, usize)>,
) -> Result<String, String> {
    let snapshot = snapshot.ok_or("No reading is selected")?;
    let value = if request.route == "apps" {
        let detail =
            crate::live_app_views::selected_detail(snapshot.detail.as_ref(), request.app.as_ref())
                .ok_or("Selected App details are still loading")?;
        let selected = selected_app_copy(detail, expanded)?;
        serde_json::json!({"second":snapshot.selection["second"],
            "observation":snapshot.selection["selected"],"detail":selected})
    } else {
        if let Some((table, index)) = expanded {
            let page = snapshot
                .pages
                .get(table)
                .ok_or("Selected table is unavailable")?;
            let row = page["rows"]
                .get(*index)
                .ok_or("Selected row is unavailable")?;
            serde_json::json!({"second":snapshot.selection["second"],
                "observation":snapshot.selection["selected"],"table":table,
                "columns":page["columns"],"values":row})
        } else {
            serde_json::json!({"second":snapshot.selection["second"],
                "observation":snapshot.selection["selected"],
                "summary":snapshot.selection["summary"]})
        }
    };
    bounded_json(&value)
}

fn selected_app_copy(detail: &Value, expanded: Option<&(String, usize)>) -> Result<Value, String> {
    if let Some((kind, index)) = expanded {
        match kind.as_str() {
            "app_processes" => {
                return detail["processes"]
                    .get(*index)
                    .cloned()
                    .ok_or_else(|| "Selected process is unavailable".into());
            }
            "executables" => {
                let groups = crate::live_executables::groups(Some(detail))?;
                return groups
                    .get(*index)
                    .map(crate::live_executables::Executable::json)
                    .ok_or_else(|| "Selected executable is unavailable".into());
            }
            _ => {}
        }
    }
    Ok(detail.clone())
}

fn bounded_json(value: &Value) -> Result<String, String> {
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    if text.len() > 1_048_576 {
        return Err("Selected details exceed the 1 MiB clipboard limit".into());
    }

    Ok(text)
}

#[cfg(test)]
#[path = "native_workflow_tests.rs"]
mod tests;

#[path = "native_workflow_probe.rs"]
mod probe;

pub fn verify_live(root: PathBuf, deadline: std::time::Instant) -> Result<Value, String> {
    probe::run(root, deadline)
}
