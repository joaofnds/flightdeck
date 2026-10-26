use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};

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
    read_command(
        std::io::stdin().as_fd(),
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap();
}

fn read_command(input: BorrowedFd<'_>, deadline: Instant) -> std::io::Result<()> {
    let mut command = [0_u8; 16];
    for index in 0..command.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(std::io::ErrorKind::TimedOut)?;
        let timeout =
            i32::try_from(remaining.as_millis()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let status = unsafe { libc::poll(&raw mut descriptor, 1, timeout) };
        if status == 0 {
            return Err(std::io::ErrorKind::TimedOut.into());
        }

        if status < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let count = unsafe { libc::read(input.as_raw_fd(), (&raw mut command[index]).cast(), 1) };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }

        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }

        if command[index] == b'\n' {
            if &command[..=index] != b"continue\n" {
                return Err(std::io::ErrorKind::InvalidInput.into());
            }

            return Ok(());
        }
    }

    Err(std::io::ErrorKind::InvalidInput.into())
}

#[test]
fn times_out_partial_diagnostic_input_with_the_writer_still_open() {
    // Goal: a readable prefix cannot extend the diagnostic command deadline.
    // Method: send one byte, keep the pipe open, and require the reader to finish independently.
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(b"c").unwrap();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let task = std::thread::spawn(move || {
        let result = read_command(reader.as_fd(), Instant::now() + Duration::from_millis(50));
        sender.send(result).unwrap();
    });

    let result = receiver.recv_timeout(Duration::from_secs(1));
    drop(writer);
    task.join().unwrap();

    assert_eq!(
        result.unwrap().unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
}

#[test]
fn accepts_complete_commands_without_consuming_the_next_gate() {
    // Goal: complete commands release their own gate and preserve queued commands for later gates.
    // Method: place two commands on one real pipe and read them independently through the adapter.
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(b"continue\ncontinue\n").unwrap();

    let deadline = Instant::now() + Duration::from_secs(1);
    for _command in 0..2 {
        read_command(reader.as_fd(), deadline).unwrap();
    }
}
