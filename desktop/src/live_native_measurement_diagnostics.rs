use std::io::{BufRead, Read, Write};

use serde_json::json;

pub(super) fn checkpoint(phase: &str) {
    if std::env::var("FLIGHTDECK_ITEM1_DIAGNOSTIC_GATE").as_deref() != Ok("1") {
        return;
    }

    println!(
        "{}",
        json!({"event": "diagnostic_gate", "phase": phase,
        "pid": std::process::id()})
    );
    std::io::stdout().flush().unwrap();
    let mut descriptor = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let status = unsafe { libc::poll(&raw mut descriptor, 1, 120_000) };
    assert_eq!(status, 1, "diagnostic gate input timeout or poll failure");
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .take(16)
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "continue\n");
}
