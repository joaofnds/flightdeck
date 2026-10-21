use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context as PollContext, Poll, Waker};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gpui::{Context, Entity};
use serde_json::Value;

use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Request, Snapshot, Update, Worker};
use crate::live_source_mode::SourceMode;
use crate::native_input::TimeInput;

gpui::actions!(flightdeck_native, [Quit, CloseWindow, OpenRecording]);

pub fn create_dashboard(
    worker: Worker,
    route: String,
    seconds: Option<u32>,
    cx: &mut gpui::App,
) -> Entity<LiveDashboard> {
    let source = worker.mode();
    let input = gpui::AppContext::new(cx, TimeInput::new);
    let entity = gpui::AppContext::new(cx, |_| {
        let mut dashboard = LiveDashboard::new(worker, route, seconds);
        dashboard.native.source = source;
        dashboard.native.input = Some(input);
        dashboard
    });

    let installed = cx.has_global::<ActionTarget>();
    cx.set_global(ActionTarget(entity.downgrade()));
    if !installed {
        install_application_actions(cx);
        install_live_actions(cx);
    }

    entity
}

/// The dashboard the application's actions act on, the one opened last. The actions are
/// installed once, since GPUI keeps every listener and key binding it is given and a window
/// may open again after it closed.
struct ActionTarget(gpui::WeakEntity<LiveDashboard>);

impl gpui::Global for ActionTarget {}

fn install_application_actions(cx: &mut gpui::App) {
    cx.bind_keys([
        gpui::KeyBinding::new("cmd-q", Quit, None),
        gpui::KeyBinding::new("cmd-w", CloseWindow, None),
        gpui::KeyBinding::new("cmd-o", OpenRecording, None),
    ]);
    cx.on_action(|_: &Quit, cx| crate::live_launch::request_quit(cx));
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            if let Err(error) = window.update(cx, |_, window, _| window.remove_window()) {
                eprintln!("Close Flightdeck window: {error}");
            }
        }
    });
    cx.set_menus(application_menus());
}

fn application_menus() -> Vec<gpui::Menu> {
    vec![
        gpui::Menu {
            name: "Flightdeck".into(),
            disabled: false,
            items: vec![
                gpui::MenuItem::action("Close Window", CloseWindow),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Quit Flightdeck", Quit),
            ],
        },
        gpui::Menu {
            name: "File".into(),
            disabled: false,
            items: vec![gpui::MenuItem::action("Open Recording…", OpenRecording)],
        },
    ]
}

fn install_live_actions(cx: &mut gpui::App) {
    cx.on_action(|_: &OpenRecording, cx| {
        let view = cx.global::<ActionTarget>().0.clone();
        if let Err(error) = view.update(cx, LiveDashboard::open_recording) {
            eprintln!("Open recording action: {error}");
        }
    });
}

type PromptPoll = Box<dyn FnMut() -> Poll<Result<Option<Vec<PathBuf>>, String>>>;

#[derive(Default)]
pub struct NativeControls {
    pub input: Option<Entity<TimeInput>>,
    pub alternative_page: u16,
    prompt: Option<PromptPoll>,
    candidate: Option<Candidate>,
    retirement: Option<RetiringReader>,
    pub notice: Option<String>,
    pub source: SourceMode,
    pub time_menu_open: bool,
}

struct Candidate {
    worker: Worker,
    request: Request,
    generation: u64,
    started: std::time::Instant,
}

struct RetiringReader {
    thread: JoinHandle<()>,
    started: Instant,
    delayed: bool,
}

const RETIREMENT_GRACE: Duration = Duration::from_secs(5);
const RETIREMENT_WAITING: &str =
    "Finishing recording cleanup. Open will become available when it finishes.";
const RETIREMENT_DELAYED: &str = "Recording cleanup is taking longer. You can keep using \
    this recording. Close and reopen Flightdeck to open another recording.";

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

    fn retire(&mut self, thread: Option<JoinHandle<()>>, now: Instant) {
        assert!(self.retirement.is_none());

        self.retirement = thread.map(|thread| RetiringReader {
            thread,
            started: now,
            delayed: false,
        });
    }

    pub fn retirement_status(&self) -> Option<&'static str> {
        self.retirement.as_ref().map(|retirement| {
            if retirement.delayed {
                RETIREMENT_DELAYED
            } else {
                RETIREMENT_WAITING
            }
        })
    }

    pub fn poll_retirement(&mut self) -> Result<bool, String> {
        self.poll_retirement_at(Instant::now())
    }

    fn poll_retirement_at(&mut self, now: Instant) -> Result<bool, String> {
        if let Some(retirement) = &mut self.retirement {
            if retirement.thread.is_finished() {
                if let Some(retirement) = self.retirement.take() {
                    retirement
                        .thread
                        .join()
                        .map_err(|_| "Reader retirement panicked".to_owned())?;

                    if matches!(
                        self.notice.as_deref(),
                        Some(RETIREMENT_WAITING | RETIREMENT_DELAYED)
                    ) {
                        self.notice = None;
                    }

                    return Ok(true);
                }

                unreachable!("A finished retirement owns its join handle");
            }

            if !retirement.delayed {
                if now.saturating_duration_since(retirement.started) >= RETIREMENT_GRACE {
                    retirement.delayed = true;

                    return Ok(true);
                }
            }
        }

        Ok(false)
    }
}

impl LiveDashboard {
    pub fn native_notice(&self, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        use gpui::prelude::*;
        let mut notices = gpui::div().children(self.native.notice.as_ref().map(|notice| {
            gpui::div()
                .id("native-notice")
                .role(gpui::Role::Status)
                .accessibility_id("flightdeck.native-notice")
                .aria_label(notice.clone())
                .px(gpui::px(24.0))
                .py(gpui::px(4.0))
                .text_color(gpui::rgb(crate::charts::MUTED))
                .child(notice.clone())
        }));
        if let Some(status) = self.native.retirement_status() {
            notices = notices.child(
                gpui::div()
                    .id("reader-retirement-status")
                    .debug_selector(|| status.into())
                    .role(gpui::Role::Status)
                    .aria_label(status)
                    .px(gpui::px(24.0))
                    .py(gpui::px(4.0))
                    .text_color(gpui::rgb(crate::charts::MUTED))
                    .child(status),
            );
        }
        if let Some((status, retry)) = crate::live_launch::collector_status(cx) {
            let mut collector = gpui::div()
                .id("collector-status")
                .role(gpui::Role::Status)
                .accessibility_id("flightdeck.collector-status")
                .aria_label(status.clone())
                .px(gpui::px(24.0))
                .py(gpui::px(4.0))
                .flex()
                .gap(gpui::px(12.0))
                .text_color(gpui::rgb(crate::charts::MUTED))
                .child(status);
            if retry {
                collector = collector.child(
                    crate::dashboard::control("Retry recording")
                        .accessibility_id("flightdeck.retry-collector")
                        .on_click(cx.listener(|_, _, _, cx| {
                            crate::live_launch::retry_collector(cx);
                        })),
                );
            }
            notices = notices.child(collector);
        }
        notices
    }

    pub fn open_recording(&mut self, cx: &mut Context<Self>) {
        if self.native.busy() {
            self.native.notice = Some(if let Some(status) = self.native.retirement_status() {
                status.into()
            } else {
                "A recording is already opening. Finish or cancel it before opening another.".into()
            });
            cx.notify();
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
        match Worker::start_recorded(path) {
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
                        self.native.retire(worker.retire(), Instant::now());
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
        let timed_out = candidate.started.elapsed() >= std::time::Duration::from_secs(30);
        match result {
            Ok(Some(update)) => {
                if self.recording_receive(update) {
                    return true;
                }
            }
            Err(error) => {
                self.cancel_recording(&format!("Recording unchanged: {error}"));
                return true;
            }
            Ok(None) => {}
        }
        if timed_out {
            self.cancel_recording("Recording unchanged: open exceeded 30 seconds");
        }

        timed_out
    }

    pub(crate) fn recording_receive(&mut self, update: Update) -> bool {
        let Some(candidate) = &self.native.candidate else {
            return false;
        };
        let expected = candidate.generation;

        match update {
            Update::Ready(snapshot) if snapshot.generation == expected => {
                self.accept_recording(snapshot);
            }
            Update::Failed {
                generation,
                message,
            } if generation == expected => {
                self.cancel_recording(&format!("Recording unchanged: {message}"));
            }
            _ => return false,
        }

        true
    }

    fn accept_recording(&mut self, snapshot: Box<Snapshot>) {
        let Some(candidate) = &self.native.candidate else {
            return;
        };
        let presentation =
            match crate::live_acceptance::Presentation::prepare(&candidate.request, snapshot) {
                Ok(presentation) => presentation,
                Err(error) => {
                    self.cancel_recording(&format!("Recording unchanged: {error}"));
                    return;
                }
            };

        if let Some(candidate) = self.native.candidate.take() {
            self.native.source = candidate.worker.mode();
            let old = std::mem::replace(&mut self.worker, candidate.worker);
            self.generation = candidate.generation;
            self.expanded = None;
            self.request.executable = None;
            self.executables.clear();
            self.hidden.clear();
            self.chart_hover = None;
            self.preview = None;
            self.picker = None;
            self.cpu_inspection = None;
            self.native.alternative_page = 0;

            self.accept_presentation(presentation);

            self.native.notice = Some("Recording opened".into());
            self.native.retire(old.retire(), Instant::now());
        }
    }

    pub fn cancel_recording(&mut self, notice: &str) {
        self.native.prompt = None;
        if let Some(candidate) = self.native.candidate.take() {
            self.native
                .retire(candidate.worker.retire(), Instant::now());
        }
        self.native.notice = Some(notice.to_owned());
    }

    pub fn inspect_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let seconds = self
            .context_snapshot()
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
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        load_process_history: true,
        earlier: crate::live_source::Earlier::Skip,
        executable: None,
    }
}

pub fn selected_copy(
    snapshot: Option<&Snapshot>,
    request: &Request,
    expanded: Option<&(String, usize)>,
) -> Result<String, String> {
    let snapshot = snapshot.ok_or("No reading is selected")?;
    // A page keeps the last selection's readings shown while the reader reads the next one.
    if let Some(second) = request.second {
        if snapshot.selection["second"].as_u64() != Some(second) {
            return Err("The selected second is still being read".into());
        }
    }
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
