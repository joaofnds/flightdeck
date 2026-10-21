//! The control socket. A test or an agent drives the running window by writing one command
//! per line to a Unix socket and reads one JSON reply per line, so checking the app never
//! takes over anyone's screen. It is built only with the `automation` feature, and its window
//! opens hidden and never takes focus. Screenshots render the window's last frame offscreen.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use gpui::{App, Entity, Modifiers, MouseButton, PlatformInput, Window, point, px, size};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::NSString;
use serde_json::{Value, json};

use crate::live_dashboard::{LiveDashboard, ROUTES};

/// The longest command line the socket reads, which bounds each read.
const LINE_LIMIT: u64 = 4096;
/// The words a command may have, which bounds its parsing.
const WORDS_MAX: usize = 8;
/// How long a connection waits for the window to answer one command.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the window looks for commands. Only automation builds pay for the wake-ups.
const POLL: Duration = Duration::from_millis(16);
/// The commands the window takes in one poll, which bounds one poll's work.
const BATCH: usize = 16;
/// How long a screenshot waits for the window to draw the commands before it.
const SETTLE: Duration = Duration::from_millis(250);
/// How often, and how many times, a screenshot looks for the page's readings to arrive, so
/// a page that never answers still gets its screenshot after five seconds.
const READING_POLL: Duration = Duration::from_millis(50);
const READING_POLLS_MAX: u32 = 100;
/// The rows the `rows` command lists, matching the dashboard's own row bound.
const ROWS_MAX: usize = 4096;
/// The accessibility nodes the `accessibility` command searches, which bounds its walk of a
/// window's tree.
const ACCESSIBILITY_NODES_MAX: usize = 65_536;

pub struct Command {
    line: String,
    reply: mpsc::Sender<String>,
}

/// Binds the socket and answers its connections on a thread of their own. A file already at
/// the path is replaced only when it is a socket, which a previous run left behind.
pub fn serve(path: &Path) -> Result<mpsc::Receiver<Command>, String> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            return Err(format!("{} exists and is not a socket", path.display()));
        }
        std::fs::remove_file(path).map_err(|error| format!("{}: {error}", path.display()))?;
    }

    let listener =
        UnixListener::bind(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("flightdeck-control".into())
        .spawn(move || accept(&listener, &sender))
        .map_err(|error| format!("control thread: {error}"))?;
    eprintln!("Control socket listening at {}", path.display());
    Ok(receiver)
}

/// Answers one connection at a time for as long as the app runs.
fn accept(listener: &UnixListener, commands: &mpsc::Sender<Command>) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = converse(stream, commands) {
                    eprintln!("Control connection: {error}");
                }
            }
            Err(error) => eprintln!("Control accept: {error}"),
        }
    }
}

fn converse(stream: UnixStream, commands: &mpsc::Sender<Command>) -> Result<(), String> {
    let mut writer = stream.try_clone().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let read = (&mut reader)
            .take(LINE_LIMIT)
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(());
        }

        let (reply, replies) = mpsc::channel();
        let command = Command {
            line: line.trim().to_owned(),
            reply,
        };
        commands
            .send(command)
            .map_err(|_| "the window has closed".to_owned())?;
        let answer = replies
            .recv_timeout(REPLY_TIMEOUT)
            .unwrap_or_else(|error| failure(&format!("no reply from the window: {error}")));
        writeln!(writer, "{answer}").map_err(|error| error.to_string())?;
    }
}

/// Runs the socket's commands on the window's foreground for as long as the window lives.
pub fn install(
    commands: mpsc::Receiver<Command>,
    dashboard: Entity<LiveDashboard>,
    window: &mut Window,
    cx: &mut App,
) {
    window
        .spawn(cx, async move |cx| {
            // The window starts from the real pointer's place on the screen, which would
            // hover whatever lies under it. A controlled window's pointer is only the socket's.
            let outside = point(px(-1.0), px(-1.0));
            let parked = cx.update(|window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseExited(gpui::MouseExitEvent {
                        position: outside,
                        pressed_button: None,
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
            });
            if parked.is_err() {
                return;
            }

            loop {
                cx.background_executor().timer(POLL).await;
                for _ in 0..BATCH {
                    let command = match commands.try_recv() {
                        Ok(command) => command,
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    };
                    let Some(answer) = respond(&command.line, &dashboard, cx).await else {
                        return;
                    };
                    if command.reply.send(answer).is_err() {
                        eprintln!("Control: the connection closed before its reply");
                    }
                }
            }
        })
        .detach();
}

/// The reply to one command, or none once the window is gone.
async fn respond(
    line: &str,
    dashboard: &Entity<LiveDashboard>,
    cx: &mut gpui::AsyncWindowContext,
) -> Option<String> {
    let words: Vec<&str> = line.split_whitespace().take(WORDS_MAX).collect();
    if words.first() == Some(&"screenshot") {
        let Some(path) = words.get(1).map(|path| (*path).to_owned()) else {
            return Some(failure("screenshot needs a path"));
        };
        let settled = settle(dashboard, cx).await?;
        // Two frames apart, so rows measured in the first frame lay out in the second.
        cx.update(draw).ok()?;
        cx.background_executor().timer(SETTLE).await;
        return cx
            .update(|window, cx| {
                draw(window, cx);
                screenshot(window, &path, settled)
            })
            .ok();
    }

    let result = cx
        .update(|window, cx| {
            let result = execute(&words, dashboard, window, cx);
            draw(window, cx);
            result
        })
        .ok()?;
    Some(match result {
        Ok(value) => json!({"ok": true, "result": value}).to_string(),
        Err(error) => failure(&error),
    })
}

/// Waits until the page holds its readings or its error, since a page reads its selection
/// on a worker after each navigation or inspection. Answers whether it did, or none once the
/// window is gone.
async fn settle(
    dashboard: &Entity<LiveDashboard>,
    cx: &mut gpui::AsyncWindowContext,
) -> Option<bool> {
    for _ in 0..READING_POLLS_MAX {
        let settled = cx
            .update(|_, cx| {
                let dashboard = dashboard.read(cx);
                if dashboard.snapshot.is_some() {
                    true
                } else {
                    dashboard.error.is_some()
                }
            })
            .ok()?;
        if settled {
            return Some(true);
        }
        cx.background_executor().timer(READING_POLL).await;
    }
    Some(false)
}

/// Draws a frame now. A hidden window gets no display refresh, so without this its hit
/// areas and its screenshots would stay at the first frame.
fn draw(window: &mut Window, cx: &mut App) {
    window.refresh();
    window.draw(cx).clear(cx);
}

fn failure(error: &str) -> String {
    json!({"ok": false, "error": error}).to_string()
}

fn screenshot(window: &Window, path: &str, settled: bool) -> String {
    let image = match window.render_to_image() {
        Ok(image) => image,
        Err(error) => return failure(&format!("render: {error}")),
    };
    match image.save(path) {
        Ok(()) => json!({"ok": true, "result": {"path": path, "width": image.width(),
            "height": image.height(), "settled": settled}})
        .to_string(),
        Err(error) => failure(&format!("{path}: {error}")),
    }
}

fn execute(
    words: &[&str],
    dashboard: &Entity<LiveDashboard>,
    window: &mut Window,
    cx: &mut App,
) -> Result<Value, String> {
    match words.first().copied().unwrap_or("") {
        "state" => Ok(state(dashboard.read(cx), window)),
        "rows" => Ok(rows(dashboard.read(cx))),
        "hovers" => Ok(dashboard.read(cx).hover_timings.report()),
        "navigate" => navigate(word(words, 1)?, dashboard, cx),
        "inspect" => {
            let second = word(words, 1)?
                .parse::<u64>()
                .map_err(|error| error.to_string())?;
            dashboard.update(cx, |dashboard, cx| dashboard.inspect(second, cx));
            Ok(Value::Null)
        }
        "live" => {
            dashboard.update(cx, |dashboard, cx| {
                if dashboard.request.second.is_some() {
                    dashboard.pause(cx);
                }
            });
            Ok(Value::Null)
        }
        "reveal" => reveal(word(words, 1)?, dashboard, cx),
        "resize" => {
            window.resize(size(px(number(words, 1)?), px(number(words, 2)?)));
            Ok(Value::Null)
        }
        "key" => {
            let keystroke =
                gpui::Keystroke::parse(word(words, 1)?).map_err(|error| error.to_string())?;
            Ok(json!({"handled": window.dispatch_keystroke(keystroke, cx)}))
        }
        "quit" => {
            crate::live_launch::request_quit(cx);
            Ok(Value::Null)
        }
        "help" => Ok(json!(HELP)),
        "drag" => drag(words, window, cx),
        "accessibility" => accessibility(words),
        _ => pointer(words, window, cx),
    }
}

const HELP: &[&str] = &[
    "state",
    "rows",
    "hovers",
    "navigate ROUTE",
    "inspect SECOND",
    "live",
    "reveal ROW_KEY",
    "resize WIDTH HEIGHT",
    "move X Y",
    "click X Y",
    "drag X Y TO_X TO_Y",
    "scroll X Y DELTA_Y",
    "exit X Y",
    "key KEYSTROKE",
    "accessibility increment|decrement TITLE",
    "screenshot PATH",
    "quit",
];

/// Pointer input at window coordinates in points, as the platform would deliver it.
fn pointer(words: &[&str], window: &mut Window, cx: &mut App) -> Result<Value, String> {
    let action = words.first().copied().unwrap_or("");
    if !matches!(action, "move" | "click" | "scroll" | "exit") {
        return Err(format!("unknown command {action:?}; try help"));
    }

    let position = point(px(number(words, 1)?), px(number(words, 2)?));
    let modifiers = Modifiers::default();
    let mut events = Vec::with_capacity(3);
    match action {
        "exit" => events.push(PlatformInput::MouseExited(gpui::MouseExitEvent {
            position,
            pressed_button: None,
            modifiers,
        })),
        "scroll" => events.push(PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
            position,
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-number(words, 3)?))),
            modifiers,
            touch_phase: gpui::TouchPhase::Moved,
        })),
        _ => events.push(PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        })),
    }
    if action == "click" {
        events.extend(click(position, modifiers));
    }

    let mut handled = false;
    for event in events {
        handled |= !window.dispatch_event(event, cx).propagate;
    }
    Ok(json!({"handled": handled}))
}

/// Presses at one point, moves to another with the button down, and releases there.
fn drag(words: &[&str], window: &mut Window, cx: &mut App) -> Result<Value, String> {
    let from = point(px(number(words, 1)?), px(number(words, 2)?));
    let to = point(px(number(words, 3)?), px(number(words, 4)?));
    let modifiers = Modifiers::default();
    let events = [
        PlatformInput::MouseDown(gpui::MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: to,
            pressed_button: Some(MouseButton::Left),
            modifiers,
        }),
        PlatformInput::MouseUp(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position: to,
            modifiers,
            click_count: 1,
        }),
    ];

    let mut handled = false;
    for event in events {
        handled |= !window.dispatch_event(event, cx).propagate;
    }
    Ok(json!({"handled": handled}))
}

fn click(position: gpui::Point<gpui::Pixels>, modifiers: Modifiers) -> [PlatformInput; 2] {
    [
        PlatformInput::MouseDown(gpui::MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        PlatformInput::MouseUp(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }),
    ]
}

fn navigate(route: &str, dashboard: &Entity<LiveDashboard>, cx: &mut App) -> Result<Value, String> {
    if !ROUTES.iter().any(|(known, _)| *known == route) {
        return Err(format!("unknown route {route:?}"));
    }

    dashboard.update(cx, |dashboard, cx| dashboard.navigate(route, cx));
    Ok(Value::Null)
}

/// Performs a screen reader's increment or decrement on the first node whose title starts
/// with the words after the action, through the window's own accessibility tree, as a screen
/// reader reaches it. The first command turns the tree on, and GPUI fills it only on the frame
/// after, so that command finds nothing.
fn accessibility(words: &[&str]) -> Result<Value, String> {
    let increment = match word(words, 1)? {
        "increment" => true,
        "decrement" => false,
        other => return Err(format!("unknown accessibility action {other:?}")),
    };
    let title = words.get(2..).unwrap_or(&[]).join(" ");
    if title.is_empty() {
        return Err("accessibility needs a title".into());
    }
    let main = MainThreadMarker::new().ok_or("accessibility runs off the main thread")?;

    let Some(node) = accessibility_node(main, &title)? else {
        return Ok(json!({"found": false}));
    };
    let performed: bool = if increment {
        // SAFETY: accessibility_node returns only nodes that answer accessibilityTitle, which
        // the NSAccessibility protocol declares together with the perform methods.
        unsafe { msg_send![&*node, accessibilityPerformIncrement] }
    } else {
        // SAFETY: as for the increment above.
        unsafe { msg_send![&*node, accessibilityPerformDecrement] }
    };
    Ok(json!({"found": true, "performed": performed}))
}

/// Searches the application's windows and their accessibility descendants, depth first, for
/// the first node whose title starts with `title`.
fn accessibility_node(
    main: MainThreadMarker,
    title: &str,
) -> Result<Option<Retained<AnyObject>>, String> {
    let application = NSApplication::sharedApplication(main);
    // SAFETY: NSApplication's windows returns an NSArray of its NSWindows.
    let windows: Retained<AnyObject> = unsafe { msg_send![&application, windows] };
    let mut pending = Vec::with_capacity(ACCESSIBILITY_NODES_MAX);
    accessibility_push(&windows, &mut pending)?;

    for _ in 0..ACCESSIBILITY_NODES_MAX {
        let Some(node) = pending.pop() else {
            return Ok(None);
        };
        // SAFETY: every NSObject answers respondsToSelector:.
        let answers: bool =
            unsafe { msg_send![&*node, respondsToSelector: sel!(accessibilityTitle)] };
        if answers {
            // SAFETY: the node answers accessibilityTitle, which returns an NSString or nil.
            let name: Option<Retained<NSString>> = unsafe { msg_send![&*node, accessibilityTitle] };
            if name.is_some_and(|name| name.to_string().starts_with(title)) {
                return Ok(Some(node));
            }
            // SAFETY: accessibilityChildren is declared beside accessibilityTitle and returns
            // an NSArray or nil.
            let children: Option<Retained<AnyObject>> =
                unsafe { msg_send![&*node, accessibilityChildren] };
            if let Some(children) = children {
                accessibility_push(&children, &mut pending)?;
            }
        }
    }
    Err(format!(
        "the accessibility tree has more than {ACCESSIBILITY_NODES_MAX} nodes"
    ))
}

/// Adds the objects of an `NSArray` to the nodes still to search, within their bound.
fn accessibility_push(
    array: &AnyObject,
    pending: &mut Vec<Retained<AnyObject>>,
) -> Result<(), String> {
    // SAFETY: the callers pass only NSArrays, which answer count.
    let count: usize = unsafe { msg_send![array, count] };
    if pending.len() + count > ACCESSIBILITY_NODES_MAX {
        return Err(format!(
            "the accessibility tree has more than {ACCESSIBILITY_NODES_MAX} nodes"
        ));
    }

    for index in 0..count {
        // SAFETY: index is below the array's count.
        let object: Retained<AnyObject> = unsafe { msg_send![array, objectAtIndex: index] };
        pending.push(object);
    }
    Ok(())
}

/// Scrolls the page so the row with this layout key starts at the top.
fn reveal(key: &str, dashboard: &Entity<LiveDashboard>, cx: &mut App) -> Result<Value, String> {
    let dashboard = dashboard.read(cx);
    let index = dashboard
        .rows
        .iter()
        .take(ROWS_MAX)
        .position(|row| row.layout_key() == key)
        .ok_or_else(|| format!("no row {key:?} on this page; try rows"))?;
    dashboard.list.scroll_to(gpui::ListOffset {
        item_ix: index,
        offset_in_item: px(0.0),
    });
    Ok(json!({"index": index}))
}

fn state(dashboard: &LiveDashboard, window: &Window) -> Value {
    let selection = dashboard
        .reading_snapshot()
        .map_or(&Value::Null, |snapshot| &snapshot.selection);
    let cards: Vec<Value> = dashboard
        .summary_cards()
        .iter()
        .map(|card| json!({"label": card.label, "reading": card.reading, "note": card.note}))
        .collect();
    let viewport = window.viewport_size();
    let top = dashboard.list.logical_scroll_top();

    json!({
        "route": dashboard.request.route,
        "requested_second": dashboard.request.second,
        "selected_second": selection["second"],
        "selected": selection["selected"],
        "status": dashboard.status(),
        "error": dashboard.error,
        "hover_second": dashboard.chart_hover.as_ref().map(|hover| hover.second),
        "target_second": dashboard.reading_target().map(|target| target.second),
        "viewport": {"width": f32::from(viewport.width), "height": f32::from(viewport.height)},
        "scale": window.scale_factor(),
        "scroll": {"row": top.item_ix, "offset": f32::from(top.offset_in_item)},
        "rows": dashboard.rows.len(),
        "renders": dashboard.renders,
        "read_ms": dashboard.snapshot.as_ref().map(|snapshot| snapshot.read_ms),
        "histories_pending": dashboard.snapshot.as_ref().map(|snapshot| snapshot.histories_pending),
        "cards": cards,
    })
}

fn rows(dashboard: &LiveDashboard) -> Value {
    let heights = dashboard.row_heights.borrow();
    let rows: Vec<Value> = dashboard
        .rows
        .iter()
        .take(ROWS_MAX)
        .enumerate()
        .map(|(index, row)| {
            let key = row.layout_key();
            let height = heights.get(&key).map(|height| f32::from(*height));
            json!({"index": index, "key": key, "height": height})
        })
        .collect();
    json!(rows)
}

fn word<'a>(words: &[&'a str], index: usize) -> Result<&'a str, String> {
    words
        .get(index)
        .copied()
        .ok_or_else(|| format!("{} needs argument {index}", words.first().unwrap_or(&"")))
}

fn number(words: &[&str], index: usize) -> Result<f32, String> {
    let value = word(words, index)?
        .parse::<f32>()
        .map_err(|error| format!("argument {index}: {error}"))?;
    if !value.is_finite() {
        return Err(format!("argument {index} is not a finite number"));
    }
    Ok(value)
}
