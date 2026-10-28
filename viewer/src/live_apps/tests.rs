use super::{Error, Frame, Member, Publication};
use crate::apps;

#[test]
fn owns_live_metadata_and_computes_known_totals_after_source_scratch_reuse() {
    // Goal: captured live frames retain bytes and exact counter baselines independently.
    // Method: capture two real v4 publications, reuse the source, then inspect transient totals.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let mut previous = None;
    fixture.capture(&mut previous);
    fixture.publish(1, Change::None);
    let mut selected = None;
    fixture.capture(&mut selected);
    fixture.publish(2, Change::None);
    let mut later = None;
    fixture.capture(&mut later);
    fixture.finish();
    let selected = selected.as_ref().unwrap();

    selected
        .with_aggregate(previous.as_deref(), |totals| {
            let app = &totals.apps[&apps::AppKey::Bundle(b"/Applications/A.app")];
            assert_eq!(app.cpu_ns.known, 500_000_000);
            assert_eq!(
                apps::cpu_percent_one_core(apps::CpuRate {
                    cpu_ns: app.cpu_ns.known,
                    elapsed_ns: totals.elapsed_ns.unwrap(),
                }),
                Some(50.0)
            );
            assert_eq!(app.energy_nj.known, 3_000_000_000);
            assert_eq!(app.resident_bytes.known, 157_286_400);
            assert_eq!(app.cpu_ns.unavailable_members, 1);
            assert!(!totals.membership_complete);
            assert_eq!(
                totals.processes[0].executable_name,
                Some(b"helper\xff".as_slice())
            );
            Ok(())
        })
        .unwrap();

    assert!(selected.bytes() >= size_of::<Frame>());
}

#[test]
fn recovers_app_rates_on_the_consecutive_tick_after_a_delayed_observation() {
    // Goal: the resumed observation has gauges only; its consecutive successor recovers rates.
    // Method: capture real-ring frames with independent ordinals and 800 ms recovery clocks.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    let times = |continuous_ns| collector::ring::ObservationTimes {
        wall_ns: 1_800_000_000_000_000_000 + continuous_ns,
        continuous_ns,
        awake_ns: continuous_ns,
    };
    fixture.publish_observed(10, &times(10_000_000_000), Change::None);
    let mut baseline = None;
    fixture.capture(&mut baseline);
    fixture.publish_observed(20, &times(20_200_000_000), Change::None);
    let mut resumed = None;
    fixture.capture(&mut resumed);
    fixture.publish_observed(21, &times(21_000_000_000), Change::None);
    let mut recovered = None;
    fixture.capture(&mut recovered);

    resumed
        .as_ref()
        .unwrap()
        .with_aggregate(baseline.as_deref(), |totals| {
            let app = &totals.apps[&apps::AppKey::Bundle(b"/Applications/A.app")];
            assert_eq!(totals.elapsed_ns, None);
            assert_eq!(app.cpu_ns.known, 0);
            assert_eq!(app.cpu_ns.unavailable_members, 3);
            assert_eq!(app.resident_bytes.known, 157_286_400);
            assert_eq!(
                totals.processes[0].cpu_ns,
                apps::Amount::Unavailable(apps::Unavailable::Interval)
            );
            Ok(())
        })
        .unwrap();
    recovered
        .as_ref()
        .unwrap()
        .with_aggregate(resumed.as_deref(), |totals| {
            let app = &totals.apps[&apps::AppKey::Bundle(b"/Applications/A.app")];
            assert_eq!(totals.elapsed_ns, Some(800_000_000));
            assert_eq!(app.cpu_ns.known, 500_000_000);
            assert_eq!(app.cpu_ns.unavailable_members, 1);
            assert_eq!(app.resident_bytes.known, 157_286_400);
            assert_eq!(
                apps::cpu_percent_one_core(apps::CpuRate {
                    cpu_ns: app.cpu_ns.known,
                    elapsed_ns: totals.elapsed_ns.unwrap(),
                }),
                Some(62.5)
            );
            Ok(())
        })
        .unwrap();
}

mod fixture;
use fixture::{Change, Fixture};

#[test]
fn refuses_an_overflowing_publication_before_optional_work_accounting() {
    // Goal: inactive instrumentation preserves the public malformed-publication refusal.
    // Method: capture one real identity publication, then supply u64::MAX to the same reader.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let mut frame = None;
    fixture.capture(&mut frame);

    let result = Frame::capture(
        &Publication {
            reader: fixture.reader.as_ref().unwrap(),
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication: u64::MAX,
        },
        &mut frame,
    );

    assert!(matches!(
        result,
        Err(Error::Bound("live App publication association"))
    ));
    assert!(frame.is_none());
}

#[test]
fn charges_the_maximum_captured_start_population_to_the_live_frame() {
    // Goal: owned Start identities and bytes stay charged to the existing observation cache.
    // Method: capture all 16384 Starts referencing one five-byte program in a real v4 ring.
    let count = collector::tracker::EVENTS_PER_TICK_MAX;
    let shape = collector::ring::Shape {
        event_capacity: u64::try_from(count).unwrap(),
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_starts(count, b"child");
    let mut frame = None;

    fixture.capture_events(&mut frame).unwrap();
    let frame = frame.as_ref().unwrap();

    assert_eq!(
        frame.bytes(),
        size_of::<Frame>() + count * size_of::<super::Start>() + count * 5
    );
    assert_eq!(frame.starts.len(), count);
    assert_eq!(frame.arena.len(), count * 5);
    eprintln!(
        "start_bytes={} count={} frame_bytes={} transient_bound={}",
        size_of::<super::Start>(),
        count,
        frame.bytes(),
        super::TRANSIENT_VIEW_BYTES_MAX
    );
}

#[test]
fn refuses_start_program_bytes_beyond_the_existing_frame_budget() {
    // Goal: event-only metadata cannot bypass the shared 32 MiB frame program-byte bound.
    // Method: publish 8195 Starts referencing a valid 4095-byte executable basename.
    let shape = collector::ring::Shape {
        event_capacity: 8195,
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_starts(8195, &[b'x'; 4095]);
    let mut frame = None;

    let result = fixture.capture_events(&mut frame);

    assert!(matches!(
        result,
        Err(Error::Bound("live App program byte bound"))
    ));
}

#[test]
fn charges_the_maximum_captured_exec_population_to_the_live_frame() {
    // Goal: every owned Exec identity and program byte is charged to the live cache.
    // Method: capture the full event bound through a real ring and inspect owned frame bytes.
    let count = collector::tracker::EVENTS_PER_TICK_MAX;
    let shape = collector::ring::Shape {
        event_capacity: u64::try_from(count).unwrap(),
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_execs(count, b"child");
    let mut frame = None;

    fixture.capture_events(&mut frame).unwrap();
    let frame = frame.as_ref().unwrap();

    assert_eq!(
        frame.bytes(),
        size_of::<Frame>() + count * size_of::<super::Exec>() + count * 5
    );
    assert_eq!(frame.execs.len(), count);
    assert_eq!(frame.arena.len(), count * 5);
    eprintln!(
        "exec_bytes={} count={} frame_bytes={} transient_bound={}",
        size_of::<super::Exec>(),
        count,
        frame.bytes(),
        super::TRANSIENT_VIEW_BYTES_MAX
    );
}

#[test]
fn refuses_exec_program_bytes_beyond_the_existing_frame_budget() {
    // Goal: Exec program metadata shares the existing 32 MiB per-frame program-byte bound.
    // Method: publish 8195 Exec observations of one valid 4095-byte basename through a real ring.
    let shape = collector::ring::Shape {
        event_capacity: 8195,
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_execs(8195, &[b'x'; 4095]);
    let mut frame = None;

    let result = fixture.capture_events(&mut frame);

    assert!(matches!(
        result,
        Err(Error::Bound("live App program byte bound"))
    ));
}

#[test]
fn retains_final_process_gauges_when_the_matching_exec_program_is_refused() {
    // Goal: unavailable App bytes cannot erase a creation-associated final numeric observation.
    // Method: capture Refused Exec metadata plus verified final usage from a real ring.
    let shape = collector::ring::Shape {
        event_capacity: 2,
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_unavailable_exec_exit();
    let mut frame = None;

    fixture.capture_events(&mut frame).unwrap();
    frame
        .as_ref()
        .unwrap()
        .with_aggregate(None, |totals| {
            assert!(totals.apps.is_empty());
            assert_eq!(totals.ungrouped_members, 1);
            assert_eq!(
                totals.processes,
                [apps::ProcessTotals {
                    pid: 7,
                    creation: Some(collector::ring::ProcessCreation {
                        unique_id: 700,
                        pid_version: 1
                    }),
                    app: None,
                    executable_name: None,
                    program_unavailable: Some(collector::ring::IdentityUnavailable::Refused),
                    cpu_ns: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    energy_nj: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    gpu_ns: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    disk_read_bytes: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    disk_written_bytes: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    disk_io_bytes: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    idle_wakeups: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    pageins: apps::Amount::Unavailable(apps::Unavailable::Interval),
                    threads: None,
                    app_attribution: None,
                    resident_bytes_last: Some(104_857_600),
                    footprint_bytes_last: Some(110_000_000),
                    state: apps::ProcessState::ExitedVerified,
                }]
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn accepts_exec_metadata_at_the_combined_program_population_bound() {
    // Goal: the shared population limit admits its last valid scheduled-plus-Exec slot.
    // Method: capture 23,999 identity-only members and one Exec, retire the source, then aggregate.
    let mut frame = None;

    capture_members_and_exec(23_999, &mut frame).unwrap();
    frame
        .as_ref()
        .unwrap()
        .with_aggregate(None, |totals| {
            assert_eq!(totals.processes.len(), 23_999);
            Ok(())
        })
        .unwrap();
}

#[test]
fn refuses_exec_metadata_past_the_combined_program_population_bound() {
    // Goal: one Exec cannot exceed the shared bound beside the largest scheduled population.
    // Method: capture 24,000 short identity-only members plus one Exec below other limits.
    let mut frame = None;

    let result = capture_members_and_exec(24_000, &mut frame);

    assert!(matches!(
        result,
        Err(Error::Bound("live App program interval bound"))
    ));
}

fn capture_members_and_exec(count: u32, frame: &mut Option<Box<Frame>>) -> super::Result<()> {
    let shape = collector::ring::Shape {
        process_capacity: u64::from(count),
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_refused_members_and_exec(count, b"child");

    fixture.capture_events(frame)
}

#[test]
fn charges_the_maximum_captured_exit_population_to_the_live_frame() {
    // Goal: publication-owned exits remain inside their explicit event and live-cache charge.
    // Method: capture all 16384 real v4 event identities, then compare their added owned bytes.
    let count = collector::tracker::EVENTS_PER_TICK_MAX;
    let shape = collector::ring::Shape {
        event_capacity: u64::try_from(count).unwrap(),
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_exits(count);
    let publication = fixture.read();
    let reader = fixture.reader.as_mut().unwrap();
    let mut events = Vec::with_capacity(count);
    let mut identities = Vec::with_capacity(count);
    for _attempt in 0..count {
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let collector::ring::FoundEvent::Event {
            number,
            tick,
            event,
        } = reader.read_event_identity(&mut identity).unwrap()
        else {
            panic!("event missing")
        };
        events.push((number, tick, event));
        identities.push(identity);
    }
    let numbered: Vec<_> = events
        .iter()
        .map(|(number, tick, event)| persistence::NumberedEvent {
            number: *number,
            tick: *tick,
            event,
        })
        .collect();
    let source = Publication {
        reader,
        tick: &fixture.tick,
        identities: &fixture.identities,
        publication,
    };
    let mut frame = None;
    Frame::capture(&source, &mut frame).unwrap();
    let frame = frame.as_mut().unwrap();
    let before = frame.bytes();

    frame
        .capture_exits(
            &source,
            &super::PublicationEvents {
                events: &numbered,
                identities: &identities,
            },
        )
        .unwrap();

    assert_eq!(frame.bytes() - before, count * size_of::<super::Exit>());
    eprintln!(
        "exit_bytes={} exit_count={} exit_charge={} frame_bytes={} transient_bound={}",
        size_of::<super::Exit>(),
        count,
        frame.bytes() - before,
        frame.bytes(),
        super::TRANSIENT_VIEW_BYTES_MAX
    );
}

#[test]
fn keeps_invalid_utf8_and_literal_backslash_program_keys_separate() {
    // Goal: live copying preserves byte identity before display escaping.
    // Method: capture two outside-bundle executables whose escaped names could collide.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let mut frame = None;
    fixture.capture(&mut frame);

    frame
        .as_ref()
        .unwrap()
        .with_aggregate(None, |totals| {
            assert_eq!(
                totals.apps[&apps::AppKey::Executable(b"name\xff")].members,
                1
            );
            assert_eq!(
                totals.apps[&apps::AppKey::Executable(b"name\\xff")].members,
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn has_no_rates_without_a_predecessor_but_keeps_selected_memory() {
    // Goal: the first retained live frame cannot invent a counter baseline.
    // Method: inspect a measured publication without an earlier cached frame.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(1, Change::None);
    let mut frame = None;
    fixture.capture(&mut frame);

    frame
        .as_ref()
        .unwrap()
        .with_aggregate(None, |totals| {
            assert!(matches!(
                totals.processes[0].cpu_ns,
                apps::Amount::Unavailable(_)
            ));
            assert_eq!(totals.processes[0].resident_bytes_last, Some(104_857_600));
            assert_eq!(
                totals.apps[&apps::AppKey::Bundle(b"/Applications/A.app")]
                    .cpu_ns
                    .known,
                0
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn retains_legacy_usage_with_unsupported_identity_and_no_pid_only_rate() {
    // Goal: legacy live sources expose raw gauges without claiming a captured App identity.
    // Method: capture two consecutive legacy publications through the same frame API.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, false);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let mut previous = None;
    fixture.capture(&mut previous);
    fixture.publish(1, Change::None);
    let mut selected = None;
    fixture.capture(&mut selected);

    selected
        .as_ref()
        .unwrap()
        .with_aggregate(previous.as_deref(), |totals| {
            assert!(totals.apps.is_empty());
            assert_eq!(totals.ungrouped_members, 4);
            assert_eq!(
                totals.processes[0].program_unavailable,
                Some(collector::ring::IdentityUnavailable::Unsupported)
            );
            assert!(matches!(
                totals.processes[0].cpu_ns,
                apps::Amount::Unavailable(_)
            ));
            assert_eq!(totals.processes[0].resident_bytes_last, Some(104_857_600));
            Ok(())
        })
        .unwrap();
}

macro_rules! creation_change {
    ($name:ident, $change:expr, $cpu:expr, $attribution:expr) => {
        #[test]
        fn $name() {
            // Goal: creation and program boundaries keep exactly the supported attribution.
            // Method: change one kernel identity field between two real live publications.
            let mut fixture = None;
            Fixture::initialize(&mut fixture, true);
            let fixture = fixture.as_mut().unwrap();
            fixture.publish(0, Change::None);
            let mut previous = None;
            fixture.capture(&mut previous);
            fixture.publish(1, $change);
            let mut selected = None;
            fixture.capture(&mut selected);

            selected
                .as_ref()
                .unwrap()
                .with_aggregate(previous.as_deref(), |totals| {
                    assert_eq!(totals.processes[0].cpu_ns, $cpu);
                    assert_eq!(totals.processes[0].app_attribution, $attribution);
                    Ok(())
                })
                .unwrap();
        }
    };
}
creation_change!(
    does_not_join_pid_reuse_to_the_previous_creation,
    Change::PidReuse,
    apps::Amount::Unavailable(apps::Unavailable::Baseline),
    None
);
creation_change!(
    retains_process_usage_across_same_path_exec_with_unknown_app_split,
    Change::Exec,
    apps::Amount::Known(200_000_000),
    Some(apps::Unavailable::Exec)
);

#[test]
fn charges_member_and_program_capacities_to_the_live_cache() {
    // Goal: retaining a frame charges its owned typed storage as well as captured bytes.
    // Method: capture five members with an independently counted 88 raw name/App bytes.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let mut frame = None;
    fixture.capture(&mut frame);
    let frame = frame.as_ref().unwrap();

    let bytes = frame.bytes();

    assert_eq!(frame.process_count(), 5);
    assert_eq!(bytes, size_of::<Frame>() + 5 * size_of::<Member>() + 88);
    assert_eq!(frame.usage_refused, 1);
}

#[test]
fn refuses_mismatching_numeric_and_identity_associations_without_retaining_partial_frame() {
    // Goal: source rows cannot silently attach one PID's usage to another captured identity.
    // Method: change a numeric PID after a successful real identity read.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let publication = fixture.read();
    let mut frame = None;
    Frame::capture(
        &Publication {
            reader: fixture.reader.as_ref().unwrap(),
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication,
        },
        &mut frame,
    )
    .unwrap();
    fixture.tick.processes[0].pid = 99;

    let result = Frame::capture(
        &Publication {
            reader: fixture.reader.as_ref().unwrap(),
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication,
        },
        &mut frame,
    );

    assert!(matches!(result, Err(Error::Bound(_))));
    assert!(frame.is_none());
}

#[test]
fn refuses_another_reader_owner_even_for_the_same_ring_generation() {
    // Goal: copied program references remain tied to the exact decoding reader.
    // Method: submit identities decoded by one reader through a newly opened reader handle.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let publication = fixture.read();
    let mut reader =
        collector::ring::RingReader::open_generation(fixture.reader.as_ref().unwrap().name())
            .unwrap();
    reader.seek_publication(publication + 1).unwrap();
    let mut frame = None;

    let result = Frame::capture(
        &Publication {
            reader: &reader,
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication,
        },
        &mut frame,
    );

    assert!(matches!(result, Err(Error::Io(_))));
    assert!(frame.is_none());
}

#[test]
fn refuses_a_frame_with_a_different_publication_cursor() {
    // Goal: capture must occur before the source advances or changes its read position.
    // Method: pair a successfully decoded tick with a different supplied publication number.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let publication = fixture.read();
    let mut frame = None;

    let result = Frame::capture(
        &Publication {
            reader: fixture.reader.as_ref().unwrap(),
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication: publication + 1,
        },
        &mut frame,
    );

    assert!(matches!(result, Err(Error::Bound(_))));
    assert!(frame.is_none());
}

#[test]
fn refuses_publication_zero_before_any_successful_source_read() {
    // Goal: a new reader cursor cannot certify an unpublished empty frame.
    // Method: submit publication zero with untouched startup reading buffers.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_ref().unwrap();
    let mut frame = None;

    let result = Frame::capture(
        &Publication {
            reader: fixture.reader.as_ref().unwrap(),
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication: 0,
        },
        &mut frame,
    );

    assert!(matches!(result, Err(Error::Bound(_))));
    assert!(frame.is_none());
}

#[test]
fn refuses_captured_metadata_exceeding_the_frame_byte_budget() {
    // Goal: valid repeated metadata cannot exceed the frame's independent 32 MiB byte bound.
    // Method: decode 8195 refused members referencing one valid 4095-byte basename.
    let shape = collector::ring::Shape {
        process_capacity: 8195,
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_refused_members(8195, &[b'x'; 4095]);
    let publication = fixture.read();
    let mut frame = None;

    let result = Frame::capture(
        &Publication {
            reader: fixture.reader.as_ref().unwrap(),
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication,
        },
        &mut frame,
    );

    assert!(matches!(result, Err(Error::Bound(_))));
    assert!(frame.is_none());
}

#[test]
fn refuses_source_capacity_above_the_domain_result_bound() {
    // Goal: a valid larger source cannot bypass the viewer's explicit operation result limit.
    // Method: decode 24001 identity-only rows within a correspondingly sized real v4 ring.
    let shape = collector::ring::Shape {
        process_capacity: 24001,
        ..fixture::SHAPE
    };
    let mut fixture = None;
    Fixture::initialize_with_shape(&mut fixture, true, &shape);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish_refused_members(24001, b"x");
    let publication = fixture.read();
    let mut frame = None;

    let result = Frame::capture(
        &Publication {
            reader: fixture.reader.as_ref().unwrap(),
            tick: &fixture.tick,
            identities: &fixture.identities,
            publication,
        },
        &mut frame,
    );

    assert!(matches!(result, Err(Error::Bound(_))));
    assert!(frame.is_none());
}

#[test]
fn refuses_to_bridge_a_missing_publication_in_the_live_cache() {
    // Goal: an available older cached frame cannot bridge an unobserved source interval.
    // Method: skip publication two and compare retained publications one and three.
    let mut fixture = None;
    Fixture::initialize(&mut fixture, true);
    let fixture = fixture.as_mut().unwrap();
    fixture.publish(0, Change::None);
    let mut previous = None;
    fixture.capture(&mut previous);
    fixture.publish(1, Change::None);
    fixture.publish(2, Change::None);
    fixture
        .reader
        .as_mut()
        .unwrap()
        .seek_publication(3)
        .unwrap();
    let mut selected = None;
    fixture.capture(&mut selected);

    selected
        .as_ref()
        .unwrap()
        .with_aggregate(previous.as_deref(), |totals| {
            assert_eq!(
                totals.processes[0].cpu_ns,
                apps::Amount::Unavailable(apps::Unavailable::Interval)
            );
            assert!(totals.elapsed_ns.is_none());
            Ok(())
        })
        .unwrap();
}
