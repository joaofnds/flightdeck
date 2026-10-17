use super::*;
use collector::ring::{IdentityAvailability, RingReader, Tick, TickBuffers};

fn tick_buffer(shape: Shape) -> Tick {
    let count = |value| usize::try_from(value).unwrap();
    Tick::from_buffers(TickBuffers {
        cpus: Vec::with_capacity(count(shape.cpu_capacity)),
        interfaces: Vec::with_capacity(count(shape.interface_capacity)),
        gpu_active_ns: Vec::with_capacity(count(shape.state_capacity)),
        core_active_ns: vec![0; count(shape.core_capacity * shape.state_capacity)],
        core_state_counts: Vec::with_capacity(count(shape.core_capacity)),
        state_capacity: count(shape.state_capacity),
        fan_rpm: Vec::with_capacity(count(shape.fan_capacity)),
        processes: Vec::with_capacity(count(shape.process_capacity)),
        coalitions: Vec::with_capacity(count(shape.coalition_capacity)),
    })
}

#[test]
fn publishes_scheduled_identity_in_warm_up_and_following_live_ticks() {
    // Goal: the actual Run caller publishes captured identity beside unchanged numeric data.
    // Method: decode warm-up and one real tick from an isolated registered source.
    let timebase = Timebase::read().unwrap();
    let recording = TestRecording::new("identityrun");
    let mut output = io::sink();
    let mut run = Run::start_with(
        timebase,
        TEST_TICK_PERIOD_NS,
        DEFAULT_RESCAN_INTERVAL,
        &recording.ring.place,
        &recording.root,
        &mut output,
        |_root, timebase| slow_persister(timebase, 3),
    )
    .unwrap();
    let mut reader = RingReader::open(&recording.ring.place.lock_path).unwrap();
    let mut tick = tick_buffer(reader.shape());
    let mut identities =
        Vec::with_capacity(usize::try_from(reader.shape().process_capacity).unwrap());

    assert_eq!(reader.source_layout_version(), 4);
    assert_eq!(reader.dictionary_capacity().unwrap().entries, 65_536);
    assert_eq!(
        reader.dictionary_capacity().unwrap().arena_bytes,
        64 * 1024 * 1024
    );
    assert_eq!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        Found::Publication {
            publication: 1,
            tick: 0
        }
    );
    assert_eq!(tick.processes, run.collector.processes.readings);
    assert_eq!(tick.processes_refused, run.collector.processes.refused);
    assert_eq!(tick.processes_gone, run.collector.processes.gone);
    assert_eq!(
        identities.len(),
        tick.processes.len()
            + usize::try_from(tick.processes_refused + tick.processes_gone).unwrap()
    );
    let own = identities
        .iter()
        .find(|value| value.pid == processes::own_pid().unwrap())
        .unwrap();
    assert_eq!(own.availability(), IdentityAvailability::Measured);
    run.tick(run.first_tick, &mut output).unwrap();

    assert!(matches!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        Found::Publication { publication: 2, .. }
    ));
    assert_eq!(tick.processes, run.collector.processes.readings);
    assert_eq!(
        identities.len(),
        run.collector.identities.observations().len()
    );
}

#[test]
fn reports_each_identity_sample_in_one_warm_up_minute_or_tail_interval() {
    // Goal: diagnostics expose raw source failures and reset without double counting.
    // Method: report known fixed counts in three successive intervals against a real ring.
    let timebase = Timebase::read().unwrap();
    let recording = TestRecording::new("identityreport");
    let run = Run::start_with(
        timebase,
        TEST_TICK_PERIOD_NS,
        DEFAULT_RESCAN_INTERVAL,
        &recording.ring.place,
        &recording.root,
        &mut io::sink(),
        |_root, timebase| slow_persister(timebase, 3),
    )
    .unwrap();
    let mut report = identity_report::IdentityReport::default();
    let mut output = Vec::new();
    let diagnostics = collector::programs::sweep::Diagnostics {
        measured: 2,
        unsupported: 1,
        readiness_unavailable: 1,
        unexpected_syscalls: 1,
        last_failure: Some(collector::programs::sweep::Failure {
            pid: 17,
            dictionary: None,
            acquired: collector::programs::Acquisition {
                readiness: Err(collector::programs::ReadinessError::Kernel { status: 5, name: 0 }),
                creation_before: Err(collector::programs::CreationError::Syscall(14)),
                ..collector::programs::Acquisition::EMPTY
            },
        }),
        ..collector::programs::sweep::Diagnostics::default()
    };

    report.add(&diagnostics);
    report.report(&run.ring, "warm-up", &mut output).unwrap();
    report.add(&collector::programs::sweep::Diagnostics {
        measured: 3,
        ..collector::programs::sweep::Diagnostics::default()
    });
    report.report(&run.ring, "minute", &mut output).unwrap();
    report.report(&run.ring, "tail", &mut output).unwrap();

    let text = String::from_utf8(output).unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("identity warm-up observations=3 measured=2"));
    assert!(lines[0].contains("gate_unavailable=1 unexpected_syscalls=1"));
    assert!(lines[1].contains("last_failure_pid=17 creation_before=Err(Syscall(14))"));
    assert!(lines[1].contains("readiness=Err(Kernel { status: 5, name: 0 })"));
    assert!(lines[2].starts_with("identity minute observations=3 measured=3"));
    assert!(lines[3].starts_with("identity tail observations=0 measured=0"));
}
