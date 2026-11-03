use super::*;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn paused_native_enumeration_and_raw_admission_expire_while_store_publishes() {
    // Goal: a stopped real reader cannot block Store publication or return an obsolete absence.
    // Method: pause inside native enumeration/read using pipes, publish, then release the reader.
    if let Some(root) = std::env::var_os("FLIGHTDECK_PUBLICATION_READER") {
        reader_child(Path::new(&root));
        return;
    }
    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    for raw in [false, true] {
        paused_reader_case(&library, raw);
    }
}
fn paused_reader_case(library: &Path, raw: bool) {
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let input = if raw {
        settle_latest_ordinary(&mut fixture, &store);
        let mut input = terminal_from_frontier(&fixture, &store);
        input.operation = AttemptOperation::Ordinary {
            window: 1,
            attempted_origin: 2,
        };
        input
    } else {
        fixture.input(false)
    };
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    let (_, attempt) = encode_original(attempt, &mut fixture, if raw { 2 } else { 1 }, &store);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    let trigger = if raw {
        let key = crate::canonical::CanonicalKey::source(
            crate::canonical::Namespace::Ordinary,
            &fixture.run,
            1,
        )
        .unwrap()
        .file_name();
        fixture.root.join("ordinary").join(&key[..1]).join(key)
    } else {
        fixture.root.join("ordinary")
    };
    let armed = fixture.base.join("reader-armed");
    fs::write(&armed, b"armed").unwrap();
    let observed = fixture.base.join("reader-observed");
    fs::write(&observed, b"").unwrap();
    let mut command = paused_reader_command(&fixture, library, raw, &trigger);
    let mut child = OwnedChild(command.spawn().unwrap());
    let paused = read_pause(child.0.stdout.as_mut().unwrap());
    finish(&mut publication);
    assert_eq!(phase(&fixture), None);
    child.0.stdin.as_mut().unwrap().write_all(b"g").unwrap();
    assert!(child.wait().success());
    if raw {
        assert!(
            !fs::read(&observed).unwrap().is_empty(),
            "selected admission must observe EIO"
        );
    }
    let mut tail = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .take(8193)
        .read_to_end(&mut tail)
        .unwrap();
    assert!(tail.len() <= 8192);
    println!(
        "reader-pause raw={raw} source_publication=Idle pause={:?} resumed={:?} work={:?}",
        String::from_utf8(paused).unwrap(),
        String::from_utf8(tail).unwrap(),
        publication.work()
    );
}
fn paused_reader_command(fixture: &Fixture, library: &Path, raw: bool, trigger: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::publication_tests::reader_tests::",
                "paused_native_enumeration_and_raw_admission_expire_while_store_publishes"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_PUBLICATION_READER", &fixture.root)
        .env(
            "FLIGHTDECK_READER_RECORDING_0",
            fixture.run.recording.0[0].to_string(),
        )
        .env(
            "FLIGHTDECK_READER_RECORDING_1",
            fixture.run.recording.0[1].to_string(),
        )
        .env("FLIGHTDECK_PAUSE_TARGET", trigger.canonicalize().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if raw {
        command
            .env(
                "FLIGHTDECK_INVENTORY_READ_TARGET",
                trigger.canonicalize().unwrap(),
            )
            .env(
                "FLIGHTDECK_REGISTRATION_ARMED",
                fixture.base.join("reader-armed"),
            )
            .env(
                "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
                fixture.base.join("reader-observed"),
            );
    }
    command
}
fn reader_child(path: &Path) {
    let recording = Identity([
        std::env::var("FLIGHTDECK_READER_RECORDING_0")
            .unwrap()
            .parse()
            .unwrap(),
        std::env::var("FLIGHTDECK_READER_RECORDING_1")
            .unwrap()
            .parse()
            .unwrap(),
    ]);
    let run = RunIdentity {
        recording,
        run: Identity([8, 9]),
        boot: Identity([10, 11]),
    };
    let root = PrivateDirectory::open(path).unwrap();
    let mut state = [0; 256];
    let memory = registration_budget();
    let error = if std::env::var_os("FLIGHTDECK_INVENTORY_READ_TARGET").is_some() {
        let mut query =
            crate::canonical::SourceFrontierRead::begin(&root, run, &mut state, &memory).unwrap();
        query.step().unwrap_err()
    } else {
        let mut cursor = crate::canonical::CanonicalCursor::begin(
            &root,
            recording,
            crate::canonical::Namespace::Ordinary,
            crate::canonical::SeekRange::source_tail(crate::canonical::Namespace::Ordinary, &run)
                .unwrap(),
            &mut state,
            &memory,
        )
        .unwrap();
        cursor.step().unwrap_err()
    };
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    println!("reader-expired={:?}", error.kind());
}
#[repr(C)]
struct PollFd {
    descriptor: i32,
    events: i16,
    returned: i16,
}
unsafe extern "C" {
    fn poll(descriptors: *mut PollFd, count: u32, timeout: i32) -> i32;
}
fn read_pause(output: &mut std::process::ChildStdout) -> Vec<u8> {
    let started = Instant::now();
    let mut bytes = Vec::with_capacity(8192);
    for _ in 0..8192 {
        let remaining = Duration::from_secs(30)
            .checked_sub(started.elapsed())
            .expect("reader pause deadline");
        let mut descriptor = PollFd {
            descriptor: output.as_raw_fd(),
            events: 1,
            returned: 0,
        };
        let ready = unsafe {
            poll(
                &raw mut descriptor,
                1,
                i32::try_from(remaining.as_millis().max(1)).unwrap(),
            )
        };
        assert_eq!(ready, 1, "native pause signal");
        let mut byte = [0; 1];
        output.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"FD-PAUSED\n") {
            return bytes;
        }
    }
    panic!("reader output bound");
}
