use super::*;
use crate::transaction::{
    AttemptInput, AttemptOperation, LossTransfer, Predecessor, UnknownProvenance, UnknownReason,
};
use collector::recording::bootstrap;
use collector::recording::private::PrivateDirectory;
use collector::ring::{RingReader, RunIdentity, Shape};
use std::io::Read;
use std::io::Write;
use std::sync::atomic::AtomicU64;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    writer: collector::ring::Writer,
    run: RunIdentity,
    transport_removed: bool,
}
impl Fixture {
    fn new(long: bool) -> Self {
        Self::new_profile(long, false)
    }
    fn new_profile(long: bool, captured: bool) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("fd-attempt-{}-{serial}", std::process::id()));
        private::create_private_dir(&base).unwrap();
        let mut root = base.join("recording");
        private::create_private_dir(&root).unwrap();
        if long {
            root = root.canonicalize().unwrap();
            for _ in 0..8 {
                let remaining = 1023 - root.as_os_str().len();
                if remaining == 0 {
                    break;
                }
                root.push("x".repeat((remaining - 1).min(200)));
                private::create_private_dir(&root).unwrap();
            }
            assert_eq!(root.as_os_str().len(), 1023);
        }
        let directory = PrivateDirectory::open(&root).unwrap();
        let lock = FileLock::try_lock_in(&directory, "registration.lock").unwrap();
        let recording = bootstrap::initialize(&directory, &lock, &mut [0; 4097]).unwrap();
        drop(lock);
        let run = RunIdentity {
            recording,
            run: Identity([8, 9]),
            boot: Identity([10, 11]),
        };
        let place = collector::ring::Place {
            lock_path: base.join("ring.lock"),
            name_prefix: format!("/fdattempt{}{serial}", std::process::id()),
        };
        let shape = Shape {
            slot_count: 1,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        };
        let mut writer = if captured {
            collector::ring::Writer::create_with_identity(
                &place,
                shape,
                collector::ring::DictionaryCapacity {
                    entries: 4,
                    arena_bytes: 16 * 1024,
                },
                &run,
                1,
            )
            .unwrap()
        } else {
            collector::ring::Writer::create_recording(&place, shape, &run, 1).unwrap()
        };
        publish_count(&mut writer, 1);
        Self {
            base,
            root,
            writer,
            run,
            transport_removed: false,
        }
    }
    fn input(&self, terminal: bool) -> AttemptInput {
        AttemptInput {
            run: self.run,
            operation: if terminal {
                AttemptOperation::Terminal { event_end: 1 }
            } else {
                AttemptOperation::Ordinary {
                    window: 0,
                    attempted_origin: 1,
                }
            },
            captured: self.writer.dictionary_usage().unwrap().is_some(),
            losses: LossTransfer {
                predecessor: Predecessor::Genesis,
                publication_start: 1,
                publication_inherited_end: 1,
                event_start: 1,
                event_inherited_end: 1,
                publications: 0,
                events: 0,
                unknown: Some(UnknownProvenance {
                    reason: UnknownReason::Predecessor,
                    run: Identity([20, 21]),
                    boot: Identity([22, 23]),
                }),
                prior_window: None,
            },
        }
    }
    fn reader(&self) -> RingReader {
        RingReader::open_generation(self.writer.name()).unwrap()
    }
    fn slot(&self) -> Vec<u8> {
        use std::io::Read;
        let mut bytes = Vec::new();
        PrivateDirectory::open(&self.root)
            .unwrap()
            .open_file("foreground")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.transport_removed {
            collector::ring::unlink_generation(self.writer.name()).unwrap();
        }
        fs::remove_dir_all(&self.base).unwrap();
    }
}

#[test]
fn dropped_registered_ordinary_keeps_its_encoded_batch_and_authority() {
    // Goal: interruption keeps both durable authority and the existing raw payload.
    // Method: encode through the registered File capability, drop Store and reopen it.
    let fixture = Fixture::new(false);
    let input = fixture.input(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    let transaction = attempt.transaction();
    let mut frame = event_frame();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(0).unwrap()).unwrap();
    let original = fs::read(
        fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(transaction).to_string())
            .join("pending/events/0000.parquet"),
    )
    .unwrap();
    let before = fixture.slot();
    drop(attempt);
    drop(store);

    let store = Store::open(&fixture.root).unwrap();
    let resumed = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    assert!(resumed.resumed());
    assert_eq!(resumed.transaction(), transaction);
    assert_eq!(fixture.slot(), before);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let data = root.open_directory("data").unwrap();
    let object = data
        .open_directory(&collector::recording::identity_text(transaction).to_string())
        .unwrap();
    let events = object
        .open_directory("pending")
        .unwrap()
        .open_directory("events")
        .unwrap();
    let mut retained = Vec::new();
    events
        .open_file("0000.parquet")
        .unwrap()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut retained)
        .unwrap();
    assert_eq!(retained, original);
    assert_eq!(
        resumed.create_file("ticks.parquet").unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fixture.slot(), before);
}

#[test]
fn terminal_registration_requires_completion_before_any_slot_change() {
    // Goal: a caller cannot certify a terminal endpoint with an unchecked boolean.
    // Method: observe refusal on a live generation, then acquire the finished source.
    let mut fixture = Fixture::new(false);
    let input = fixture.input(true);
    let store = Store::open(&fixture.root).unwrap();
    let before = fixture.slot();
    assert_eq!(
        store
            .register_attempt(&input, &mut fixture.reader(), &registration_budget())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fixture.slot(), before);
    fixture.writer.finish();
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    let transaction = attempt.transaction();
    drop(attempt);
    let reopened = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    assert_eq!(reopened.transaction(), transaction);
    assert_eq!(reopened.input().losses.unknown, input.losses.unknown);
}

#[test]
fn longest_root_uses_registered_descriptor_payloads() {
    // Goal: new raw locators do not shorten the admitted root path.
    // Method: write a real Parquet batch beyond PATH_MAX via retained directory Files.
    let fixture = Fixture::new(true);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(
        &mut event_frame(),
        attempt.create_event_batch(4095).unwrap(),
    )
    .unwrap();
    assert!(attempt.create_file("../escape").is_err());
    assert!(attempt.create_file("events.parquet").is_err());
}

#[test]
fn conflicting_attempt_and_uncomposed_worker_frontier_leave_slot_unchanged() {
    // Goal: only a matching registered owner can regain an occupied attempt.
    // Method: change the attempted window and enter the normal worker progress owner.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let input = fixture.input(false);
    drop(
        store
            .register_attempt(&input, &mut fixture.reader(), &registration_budget())
            .unwrap(),
    );
    let before = fixture.slot();
    let mut conflict = input;
    conflict.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: 1,
    };
    assert_eq!(
        store
            .register_attempt(&conflict, &mut fixture.reader(), &registration_budget())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        store.progress(&fixture.run).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fixture.slot(), before);
}

fn event_frame() -> polars::prelude::DataFrame {
    crate::event_recovery::batch::frame(
        &[crate::event_recovery::batch::Row {
            publication: None,
            event: crate::window::EventRecord {
                number: 1,
                tick: 1,
                pid: 41,
                kind: 1,
                coalition: None,
                usage: None,
                availability: crate::schema::availability::NOT_APPLICABLE,
                noticed: None,
            },
            identity: None,
            name: None,
            app: None,
        }],
        false,
    )
    .unwrap()
}

#[test]
fn ordinary_registration_preserves_scanning_loss_and_unknown_provenance() {
    // Goal: scanning's proved loss prefix is durable before a payload can be staged.
    // Method: wrap the one-slot source, register its real Missed interval, then reopen.
    let mut fixture = Fixture::new(false);
    publish_three(&mut fixture.writer);
    let mut input = fixture.input(false);
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 4,
    };
    input.losses.publication_inherited_end = 4;
    input.losses.publications = 3;
    let store = Store::open(&fixture.root).unwrap();
    drop(
        store
            .register_attempt(&input, &mut fixture.reader(), &registration_budget())
            .unwrap(),
    );
    let (transaction, decoded) =
        AttemptInput::decode_registered(&fixture.slot(), fixture.run.recording).unwrap();
    assert_eq!(decoded, input);
    let resumed = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    assert_eq!(resumed.transaction(), transaction);
}

#[test]
fn terminal_registration_preserves_prebegin_scanning_loss() {
    // Goal: terminal registration transfers the same loss owner before creating payloads.
    // Method: wrap publications, finish the exact generation and decode its occupied slot.
    let mut fixture = Fixture::new(false);
    publish_three(&mut fixture.writer);
    fixture.writer.finish();
    let mut input = fixture.input(true);
    input.losses.publication_inherited_end = 4;
    input.losses.publications = 3;
    let store = Store::open(&fixture.root).unwrap();
    drop(
        store
            .register_attempt(&input, &mut fixture.reader(), &registration_budget())
            .unwrap(),
    );
    assert_eq!(
        AttemptInput::decode_registered(&fixture.slot(), fixture.run.recording)
            .unwrap()
            .1,
        input
    );
}

fn publish_three(writer: &mut collector::ring::Writer) {
    publish_count(writer, 3);
}

fn publish_count(writer: &mut collector::ring::Writer, count: u64) {
    let memory = collector::machine::Memory::default();
    for run_tick in 1..=count {
        let readings = collector::ring::Readings {
            run_tick,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: collector::ring::Dropped::default(),
        };
        let times = collector::ring::ObservationTimes {
            wall_ns: run_tick,
            continuous_ns: run_tick,
            awake_ns: run_tick,
        };
        if writer.dictionary_usage().unwrap().is_some() {
            writer
                .publish_with_identity(
                    &readings,
                    Some(&times),
                    &collector::ring::IdentityPublication {
                        processes: &[],
                        events: &[],
                    },
                )
                .unwrap();
        } else {
            writer.publish_observed(&readings, &times);
        }
    }
}

#[test]
fn native_registration_barriers_report_failure_and_reconcile_visible_effects() {
    // Goal: native durability failures cannot authorize staging ahead of registration.
    // Method: fail pending-file, root-directory and raw-parent full-sync in isolated children.
    if let Some(root) = std::env::var_os("FLIGHTDECK_REGISTRATION_ROOT") {
        native_sync_child(Path::new(&root));
        return;
    }
    let fixture = Fixture::new(false);
    let library = native_registration_build(&fixture);
    for boundary in ["foreground.pending", "root", "data"] {
        // Each boundary has its own recording, because a visible occupied slot cannot reset.
        let case = Fixture::new(false);
        let mut bytes = [0; 4096];
        case.input(false)
            .encode_registered(Identity([1, 1]), &mut bytes)
            .unwrap();
        fs::write(case.base.join("input"), bytes).unwrap();
        fs::write(case.base.join("observed"), b"").unwrap();
        let canonical = case.root.canonicalize().unwrap();
        let target = if boundary == "root" {
            canonical
        } else {
            canonical.join(boundary)
        };
        let mut child = OwnedChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    concat!(
                        "store::registration_tests::",
                        "native_registration_barriers_report_failure_and_reconcile_visible_effects"
                    ),
                    "--nocapture",
                ])
                .env("DYLD_INSERT_LIBRARIES", &library)
                .env("FLIGHTDECK_REGISTRATION_ROOT", &case.root)
                .env("FLIGHTDECK_REGISTRATION_BASE", &case.base)
                .env("FLIGHTDECK_REGISTRATION_RING", case.writer.name())
                .env("FLIGHTDECK_REGISTRATION_BOUNDARY", boundary)
                .env("FLIGHTDECK_REGISTRATION_ARMED", case.base.join("armed"))
                .env(
                    "FLIGHTDECK_PRIVATE_SYNC_TARGET",
                    target.canonicalize().unwrap_or(target),
                )
                .env(
                    "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
                    case.base.join("observed"),
                )
                .spawn()
                .unwrap(),
        );
        assert!(child.wait().success(), "boundary {boundary}");
    }
}

fn native_registration_build(fixture: &Fixture) -> PathBuf {
    let library = fixture.base.join("registration-sync.dylib");
    let log_path = fixture.base.join("native-build.log");
    let log = File::create(&log_path).unwrap();
    let mut build = OwnedChild(
        std::process::Command::new("rustc")
            .args(["--edition=2024", "--crate-type=cdylib", "--deny=warnings"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/registration_sync_fault.rs"))
            .arg("-o")
            .arg(&library)
            .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(log))
            .spawn()
            .unwrap(),
    );
    assert!(
        build.wait().success(),
        "{}",
        fs::read_to_string(log_path).unwrap()
    );
    library
}

struct OwnedChild(std::process::Child);

impl OwnedChild {
    fn wait(&mut self) -> std::process::ExitStatus {
        let started = std::time::Instant::now();
        for _ in 0..3000 {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(30));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("registration child exceeded wait bound");
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

fn native_sync_child(root: &Path) {
    let base = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_BASE").unwrap());
    let boundary = std::env::var("FLIGHTDECK_REGISTRATION_BOUNDARY").unwrap();
    let bytes = fs::read(base.join("input")).unwrap();
    let recording = Identity([
        u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
        u64::from_be_bytes(bytes[24..32].try_into().unwrap()),
    ]);
    let (_, input) = AttemptInput::decode_registered(&bytes, recording).unwrap();
    let store = Store::open(root).unwrap();
    let before = fs::read(root.join("foreground")).unwrap();
    let name = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    fs::write(base.join("armed"), b"arm").unwrap();
    let error = store
        .register_attempt(
            &input,
            &mut RingReader::open_generation(&name).unwrap(),
            &registration_budget(),
        )
        .err()
        .unwrap();
    assert_eq!(error.raw_os_error(), Some(5));
    assert!(!fs::read(base.join("observed")).unwrap().is_empty());
    fs::remove_file(base.join("armed")).unwrap();
    let after = fs::read(root.join("foreground")).unwrap();
    if boundary == "foreground.pending" {
        assert_eq!(after, before);
        let directory = PrivateDirectory::open(root).unwrap();
        for name in ["data", "recovery-data"] {
            directory
                .open_directory(name)
                .unwrap()
                .visit_names(0, |_| panic!("orphan raw artifact before registration"))
                .unwrap();
        }
        let attempt = store
            .register_attempt(
                &input,
                &mut RingReader::open_generation(&name).unwrap(),
                &registration_budget(),
            )
            .unwrap();
        assert!(!attempt.resumed());
        let expected = collector::recording::identity_text(attempt.transaction()).to_string();
        let mut count = 0;
        directory
            .open_directory("data")
            .unwrap()
            .visit_names(1, |name| {
                assert_eq!(name, expected);
                count += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 1);
        directory
            .open_directory("recovery-data")
            .unwrap()
            .visit_names(0, |_| panic!("foreign terminal object"))
            .unwrap();
    } else {
        let (transaction, decoded) = AttemptInput::decode_registered(&after, recording).unwrap();
        assert_eq!(decoded, input);
        let resumed = store.resume_attempt().unwrap();
        assert!(resumed.resumed());
        assert_eq!(resumed.transaction(), transaction);
        assert_eq!(fs::read(root.join("foreground")).unwrap(), after);
    }
}

#[test]
fn malformed_occupied_authority_refuses_without_replacement() {
    // Goal: structural parsing cannot authorize an unsupported or malformed payload.
    // Method: corrupt occupied slot fields and compare bytes and inode after each refusal.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    drop(
        store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap(),
    );
    let valid = fixture.slot();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    for offset in [48, 52, 56, 120, 132, 224, 1088, 1343, 2112] {
        let mut damaged = valid.clone();
        damaged[offset] ^= 128;
        private::atomic_write_in(&root, "foreground", &damaged).unwrap();
        let inode = root
            .open_file("foreground")
            .unwrap()
            .metadata()
            .unwrap()
            .ino();
        assert!(store.resume_attempt().is_err(), "offset {offset}");
        assert_eq!(fixture.slot(), damaged);
        assert_eq!(
            root.open_file("foreground")
                .unwrap()
                .metadata()
                .unwrap()
                .ino(),
            inode
        );
    }
}

#[test]
fn raw_substitution_and_foreign_object_name_are_retained_on_refusal() {
    // Goal: the inventory capability cannot follow a symlink or normalize foreign contents.
    // Method: substitute an admitted file and add an unknown object child, then reopen.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let transaction = attempt.transaction();
    drop(attempt);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let foreign = fixture.base.join("foreign");
    fs::write(&foreign, b"unchanged").unwrap();
    std::os::unix::fs::symlink(&foreign, object.join("pending/ticks.parquet")).unwrap();
    let resumed = store.resume_attempt().unwrap();
    assert!(resumed.open_file("ticks.parquet").is_err());
    drop(resumed);
    let before = fixture.slot();
    private::open_private_file(&object.join("foreign"), true).unwrap();
    assert!(store.resume_attempt().is_err());
    assert_eq!(fixture.slot(), before);
    assert_eq!(fs::read(foreign).unwrap(), b"unchanged");
    assert!(object.join("foreign").exists());
}

#[test]
fn resumed_authority_does_not_require_the_original_transport() {
    // Goal: transport loss cannot erase already registered provenance or original bytes.
    // Method: remove the exact generation after registration and reopen through Store alone.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    drop(
        store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap(),
    );
    drop(store);
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    assert!(RingReader::open_generation(fixture.writer.name()).is_err());
    let store = Store::open(&fixture.root).unwrap();
    let resumed = store.resume_attempt().unwrap();
    assert!(resumed.resumed());
    assert_eq!(resumed.input(), &fixture.input(false));
}

#[test]
fn unfinished_slot_scratch_refuses_without_deleting_it() {
    // Goal: generic replacement cannot erase unchecked transaction scratch.
    // Method: leave private truncated scratch beside Idle and compare bytes after begin.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    root.create_file("foreground.pending")
        .unwrap()
        .write_all(b"FDTXN002")
        .unwrap();
    let before = fixture.slot();
    assert_eq!(
        store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget()
            )
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fixture.slot(), before);
    assert_eq!(
        fs::read(fixture.root.join("foreground.pending")).unwrap(),
        b"FDTXN002"
    );
    root.open_directory("data")
        .unwrap()
        .visit_names(0, |_| panic!("raw artifact preceded registration"))
        .unwrap();
}

#[test]
fn canonical_loss_predecessor_establishes_the_next_unowned_domains() {
    // Goal: real Store registration consumes canonical source ownership after genesis.
    // Method: install an independently encoded loss gate with Idle slots, then register its
    // successor.
    let mut fixture = Fixture::new(false);
    publish_count(&mut fixture.writer, 1);
    let transaction = Identity([30, 31]);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    private::create_private_dir(&object).unwrap();
    private::create_private_dir(&object.join("loss")).unwrap();
    let prefix = canonical_loss_prefix(fixture.run, transaction);
    let metadata = object.join("loss/metadata");
    private::open_private_file(&metadata, true)
        .unwrap()
        .write_all(&prefix)
        .unwrap();
    let key = format!(
        "{}{}{:016x}.seal",
        collector::recording::identity_text(fixture.run.run),
        collector::recording::identity_text(fixture.run.boot),
        1
    );
    fs::hard_link(&metadata, fixture.root.join("ordinary").join(key)).unwrap();
    let mut input = fixture.input(false);
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 2,
    };
    input.losses.predecessor = Predecessor::Canonical {
        transaction,
        kind: crate::transaction::CanonicalKind::LossOnly,
        origin: 1,
    };
    input.losses.publication_start = 2;
    input.losses.publication_inherited_end = 2;
    let store = Store::open(&fixture.root).unwrap();
    let result = store.register_attempt(&input, &mut fixture.reader(), &registration_budget());
    assert!(
        result.is_ok(),
        "canonical predecessor refused: {:?}",
        result.err()
    );
}

fn canonical_loss_prefix(run: RunIdentity, transaction: Identity) -> [u8; 512] {
    let mut prefix = [0_u8; 512];
    prefix[..8].copy_from_slice(b"FDCAN002");
    for (offset, value) in [(8, 1_u32), (12, 4), (80, 1), (84, 3), (168, 3), (264, 1)] {
        prefix[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (offset, identity) in [
        (16, run.recording),
        (32, run.run),
        (48, run.boot),
        (64, transaction),
        (88, transaction),
    ] {
        prefix[offset..offset + 8].copy_from_slice(&identity.0[0].to_be_bytes());
        prefix[offset + 8..offset + 16].copy_from_slice(&identity.0[1].to_be_bytes());
    }
    prefix[256..264].copy_from_slice(b"FDLoss02");
    for (offset, value) in [
        (112, 1_u64),
        (128, 2),
        (136, 1),
        (144, 1),
        (304, 1),
        (312, 1),
        (320, 1),
        (328, 1),
        (352, 2),
        (360, 1),
        (368, 1),
    ] {
        prefix[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    prefix
}

#[test]
fn prepared_frontier_requires_the_same_root_and_current_snapshot() {
    // Goal: a completed read cannot become freely forged or stale registration authority.
    // Method: drive the real read continuation, then test foreign-root and changed-state refusal.
    let first = Fixture::new(false);
    let second = Fixture::new(false);
    let store = Store::open(&first.root).unwrap();
    let other = Store::open(&second.root).unwrap();
    let root = PrivateDirectory::open(&first.root).unwrap();
    let memory = Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024));
    let mut state = [0; 256];
    let mut query =
        crate::canonical::SourceFrontierRead::begin(&root, first.run, &mut state, &memory).unwrap();
    assert_eq!(
        query.result().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    for _ in 0..3 {
        query.step().unwrap();
    }
    assert!(query.result().unwrap().is_none());
    assert!(
        other
            .register_attempt_from_frontier(&second.input(false), &mut second.reader(), query)
            .is_err()
    );
    let before = first.slot();
    let mut state = [0; 256];
    let mut query =
        crate::canonical::SourceFrontierRead::begin(&root, first.run, &mut state, &memory).unwrap();
    for _ in 0..3 {
        query.step().unwrap();
    }

    let mut changed = [0; 256];
    bootstrap::codec::encode_state(
        first.run.recording,
        &bootstrap::codec::NamespaceState::Quiet { sequence: 1 },
        &mut changed,
    )
    .unwrap();
    private::atomic_write_in(&root, "namespace-state", &changed).unwrap();
    assert_eq!(
        store
            .register_attempt_from_frontier(&first.input(false), &mut first.reader(), query)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(first.slot(), before);
    let mut state = [0; 256];
    let mut query =
        crate::canonical::SourceFrontierRead::begin(&root, first.run, &mut state, &memory).unwrap();
    for _ in 0..3 {
        query.step().unwrap();
    }
    let attempt = store
        .register_attempt_from_frontier(&first.input(false), &mut first.reader(), query)
        .unwrap();
    assert!(!attempt.resumed());
}

fn gap_successor(fixture: &Fixture, saved: u64) -> AttemptInput {
    let transaction = Identity([30, 31]);
    let mut prefix = canonical_loss_prefix(fixture.run, transaction);
    for (offset, value) in [
        (104, saved + 1),
        (112, 2),
        (128, 3),
        (272, 20),
        (280, 21),
        (288, 1),
        (296, 1),
        (304, 2),
        (312, 2),
        (352, 3),
        (424, saved),
    ] {
        prefix[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    prefix[432..436].copy_from_slice(&1_u32.to_be_bytes());
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    private::create_private_dir(&object).unwrap();
    private::create_private_dir(&object.join("loss")).unwrap();
    let metadata = object.join("loss/metadata");
    private::open_private_file(&metadata, true)
        .unwrap()
        .write_all(&prefix)
        .unwrap();
    let key = crate::canonical::CanonicalKey::source(
        crate::canonical::Namespace::Ordinary,
        &fixture.run,
        2,
    )
    .unwrap();
    fs::hard_link(
        metadata,
        fixture.root.join("ordinary").join(key.file_name()),
    )
    .unwrap();
    let mut input = fixture.input(false);
    input.losses.predecessor = Predecessor::Canonical {
        transaction,
        kind: crate::transaction::CanonicalKind::LossOnly,
        origin: 2,
    };
    input.losses.prior_window = Some(9);
    input.losses.publication_start = 3;
    input.losses.publication_inherited_end = 3;
    input.operation = AttemptOperation::Ordinary {
        window: 10,
        attempted_origin: 3,
    };
    input
}
fn publish_window_ten(writer: &mut collector::ring::Writer) {
    let memory = collector::machine::Memory::default();
    for _ in 0..2 {
        writer.publish(&collector::ring::Readings {
            run_tick: 600,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: collector::ring::Dropped::default(),
        });
    }
}
#[test]
fn a_gap_in_window_ten_still_allows_its_first_row_bearing_attempt() {
    // Goal: a loss-only prefix carries saved window9 rather than claiming window10 was saved.
    // Method: register actual window10 timing against the canonical gap frontier.
    let mut fixture = Fixture::new(false);
    publish_window_ten(&mut fixture.writer);
    let input = gap_successor(&fixture, 9);
    let store = Store::open(&fixture.root).unwrap();
    assert!(
        store
            .register_attempt(&input, &mut fixture.reader(), &registration_budget())
            .is_ok()
    );
}
#[test]
fn intervening_gaps_cannot_authorize_a_second_saved_window_ten() {
    // Goal: caller counters cannot roll the canonical last-window fact backwards.
    // Method: claim prior9 after a gap that durably carries saved10 and compare unchanged slot
    // bytes.
    let mut fixture = Fixture::new(false);
    publish_window_ten(&mut fixture.writer);
    let input = gap_successor(&fixture, 10);
    let store = Store::open(&fixture.root).unwrap();
    let before = fixture.slot();
    assert_eq!(
        store
            .register_attempt(&input, &mut fixture.reader(), &registration_budget())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fixture.slot(), before);
}

#[test]
fn resumed_ready_remains_the_only_original_placement() {
    // Goal: an interrupted ready transition cannot be hidden by a new empty pending.
    // Method: rename the registered original, remove transport, and resume Store.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let transaction = attempt.transaction();
    drop(attempt);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    fs::rename(object.join("pending"), object.join("ready")).unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;

    let resumed = store.resume_attempt().unwrap();
    assert!(resumed.resumed());
    assert!(!object.join("pending").exists());
    assert!(object.join("ready").is_dir());
}

#[path = "inventory_tests.rs"]
mod inventory_tests;

fn registration_budget() -> Arc<crate::resources::ByteBudget> {
    Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024))
}

#[test]
fn registration_uses_and_releases_the_callers_shared_allowance() {
    // Goal: registration cannot borrow an independent memory ceiling.
    // Method: exhaust the caller ledger before Idle admission and compare durable/raw state.
    let mut fixture = Fixture::new(false);
    publish_three(&mut fixture.writer);
    let input = budget_history(&fixture);
    let store = Store::open(&fixture.root).unwrap();
    let memory = registration_budget();
    let held = memory.reserve(memory.maximum()).unwrap();
    let before = fixture.slot();
    assert_eq!(
        store
            .register_attempt(&input, &mut fixture.reader(), &memory)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::OutOfMemory
    );
    assert_eq!(fixture.slot(), before);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    root.open_directory("data")
        .unwrap()
        .visit_names(1, |name| {
            assert_eq!(
                name,
                collector::recording::identity_text(Identity([50, 51])).to_string()
            );
            Ok(())
        })
        .unwrap();
    drop(held);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &memory)
        .unwrap();
    assert_eq!(memory.used(), 0);
    assert!(memory.used_peak() > 0);
    drop(attempt);
}

fn budget_history(fixture: &Fixture) -> AttemptInput {
    let transaction = Identity([50, 51]);
    let mut prefix = canonical_loss_prefix(fixture.run, transaction);
    for (offset, value) in [(128, 4_u64), (352, 4), (368, 3)] {
        prefix[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    private::create_private_dir(&object).unwrap();
    let loss = object.join("loss");
    private::create_private_dir(&loss).unwrap();
    private::open_private_file(&loss.join("metadata"), true)
        .unwrap()
        .write_all(&prefix)
        .unwrap();
    let key = crate::canonical::CanonicalKey::source(
        crate::canonical::Namespace::Ordinary,
        &fixture.run,
        1,
    )
    .unwrap();
    fs::hard_link(
        loss.join("metadata"),
        fixture.root.join("ordinary").join(key.file_name()),
    )
    .unwrap();
    let mut input = fixture.input(false);
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 4,
    };
    input.losses.predecessor = Predecessor::Canonical {
        transaction,
        kind: crate::transaction::CanonicalKind::LossOnly,
        origin: 1,
    };
    input.losses.publication_start = 4;
    input.losses.publication_inherited_end = 4;
    input
}
