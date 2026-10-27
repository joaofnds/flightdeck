#[test]
fn does_not_report_a_cancelled_raw_capture_as_complete() {
    // Goal: a cancelled candidate contributes attempted work but no completed raw capture.
    // Method: reuse a real publication and interrupt its operation before owned capture ends.
    use persistence::work::ingestion::{Kind, Trace};
    let mut fixture = RingFixture::new_following();
    fixture.publish_following(1);
    let session = LiveSession::open(&fixture.root).unwrap();
    let source = &session.sources[0];
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    cancellation.cancel().unwrap();
    let trace = Trace::new(source.identity.recording);
    let scope = trace.enter();
    let mut raw = None;

    let result = Raw::capture(
        &Publication {
            reader: &source.reader,
            tick: &source.tick,
            identities: &source.identities,
            publication: 1,
        },
        &PublicationEvents {
            events: &[],
            identities: &[],
        },
        &operation,
        &mut raw,
    );
    drop(scope);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();

    assert!(matches!(result,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == Kind::RawCapture)
            .count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == Kind::RawCaptureAttempt)
            .count(),
        1
    );
}

#[test]
fn interrupts_a_page_after_formatting_starts_and_retries_in_a_new_epoch() {
    // Goal: superseding a page after its first formatted row never returns a stale response.
    // Method: interrupt at a deterministic row boundary, then retrieve exact cells in a new epoch.
    let mut fixture = RingFixture::new_following();
    fixture.publish_following(1);
    let cancellation = Arc::new(Cancellation::default());
    let mut session = LiveSession::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    session.follow_latest().unwrap();
    cancellation.interrupt_after_page_row();

    let error = session.page("fans", 0).unwrap_err();

    assert!(matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(cancellation.epoch(), 1);
    session.begin(cancellation.epoch()).unwrap();
    let selection = session.follow_latest().unwrap();
    let page = session.page("fans", 0).unwrap();
    assert_eq!(page["observation"], selection["selected"]);
    assert_eq!(page["rows"], json!([["1", "0", "1"], ["1", "1", "0"]]));
}

#[test]
fn keeps_machine_decimal_presentation_and_multiplication_first_rates() {
    // Goal: native reduction preserves the legacy public decimal and rate operation order.
    // Method: submit a real decoded ring tick with non-binary f32 gauges and a 1 ns offset.
    let mut fixture = RingFixture::new_following();
    fixture.publish_following(1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    fixture.publish_following(2);
    assert!(matches!(
        session.sources[0].read_tick().unwrap(),
        Found::Publication { .. }
    ));
    let tick = &mut session.sources[0].tick;
    tick.power_watts = 0.1;
    tick.soc_celsius = 40.2;
    tick.fan_rpm[0] = 1234.1;
    tick.memory.pageins = 1;
    tick.times.as_mut().unwrap().continuous_ns = 1_000_000_001;

    session.poll_publication(0, 2).unwrap();
    let response = session.follow_latest().unwrap();

    assert_eq!(response["summary"]["power_watts"], 0.1);
    assert_eq!(response["summary"]["soc_celsius"], 40.2);
    assert_eq!(response["summary"]["fan_rpm"][0], 1234.1);
    let expected: f64 = 0.999_999_999;
    assert_eq!(
        response["summary"]["memory_pageins_per_second"]
            .as_f64()
            .unwrap()
            .to_bits(),
        expected.to_bits()
    );
    let app_expected: f64 = 0.999_999_998_999_999_9;
    assert_eq!(
        model::rates::bytes_per_second(model::rates::BytesRate {
            bytes: 1,
            elapsed_ns: 1_000_000_001,
        })
        .unwrap()
        .to_bits(),
        app_expected.to_bits()
    );
    assert_eq!(session.page("fans", 0).unwrap()["rows"][0][2], "1234.1");
}

#[test]
fn preserves_populated_live_core_level_states_without_retaining_a_table() {
    // Goal: the extra public live page survives typed retention and leaves saved behavior intact.
    // Method: publish four real cores, attach decoded level facts, then page and seal that tick.
    let mut fixture = RingFixture::new_with_shape(&Shape {
        slot_count: 2,
        cpu_capacity: 4,
        interface_capacity: 1,
        core_capacity: 4,
        state_capacity: 2,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 8,
    });
    let mut session = LiveSession::open(&fixture.root).unwrap();
    typed_core_publish(&mut fixture);
    assert!(matches!(
        session.sources[0].read_tick().unwrap(),
        Found::Publication { .. }
    ));
    let level = |name: &[u8]| collector::system::PerformanceLevel {
        name: collector::system::FactText::new(name).unwrap(),
        logical_cpus: 2,
    };
    session.sources[0].tick.system = Some(collector::ring::SystemSnapshot {
        machine: collector::system::MachineFacts {
            levels: Some([level(b"Fast"), level(b"Slow")]),
            ..collector::system::MachineFacts::default()
        },
        ..collector::ring::SystemSnapshot::default()
    });
    session.poll_publication(0, 1).unwrap();
    session.follow_latest().unwrap();

    let page = session.page("core_level_states", 0).unwrap();

    assert_eq!(
        page["columns"],
        json!([
            "publication",
            "level",
            "state",
            "active_ns",
            "frequency_khz"
        ])
    );
    assert_eq!(page["total"], 4);
    assert_eq!(
        page["rows"],
        json!([
            ["1", "0", "0", "12", null],
            ["1", "0", "1", "14", null],
            ["1", "1", "0", "4", null],
            ["1", "1", "1", "6", null]
        ])
    );
    let mut window = Window::new(
        WindowIdentity {
            run: session.sources[0].identity,
            window: 0,
        },
        Limits::from_shape(&session.sources[0].reader.shape()).unwrap(),
    );
    cpu_detail_stage(&mut window, &session.sources[0], 1);
    seal_window(&fixture.root, &window);
    let mut saved = Session::open(&fixture.root).unwrap();
    saved.select(1000, 0).unwrap();
    assert!(saved.page("core_level_states", 0).is_err());
}

fn typed_core_publish(fixture: &mut RingFixture) {
    let cores = [[1, 2], [3, 4], [5, 6], [7, 8]]
        .map(|states| collector::ioreport::States::with_active_ns(&states));
    fixture.writer.publish_observed(
        &Readings {
            run_tick: 1,
            cpus: &[collector::machine::CPUTicks::default(); 4],
            memory: &collector::machine::Memory::default(),
            interfaces: &[],
            ioreport: Some(collector::ring::IOReportReadings {
                gpu_energy_nj: 0,
                gpu_active_ns: &[0, 0],
                cores: &cores,
            }),
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        },
        &ObservationTimes {
            wall_ns: 1_000_000_000_000,
            continuous_ns: 0,
            awake_ns: 0,
        },
    );
}

#[test]
fn charges_native_owners_and_fixed_cache_storage_without_counting_inline_raw_twice() {
    // Goal: cache accounting includes each owned capacity once and reserves its fixed slots.
    // Method: enumerate independently the retained owners of four real identity publications.
    let mut fixture = identity_fixture::Fixture::new();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    for second in 1000..1004 {
        fixture.publish_with_system_rows(second);
        session.poll().unwrap();
    }
    let fixed = session.observations.capacity() * size_of::<LiveObservation>()
        + session.points.capacity() * size_of::<Value>();
    let mut expected = fixed;
    for observation in &session.observations {
        expected += observation.raw.bytes() - size_of::<Raw>()
            + observation.app_frame.bytes()
            + observation.app_sample.as_ref().unwrap().bytes()
            + point_bytes::charge(&observation.point).unwrap();
    }
    for point in &session.points {
        expected += point_bytes::charge(point).unwrap();
    }

    assert_eq!(session.cache_bytes, expected);
    assert!(expected <= CACHE_BYTES_MAX);
    println!(
        "{}",
        json!({"cache_charge": expected, "fixed_slot_bytes": fixed,
        "observations": session.observations.len(), "candidate_bytes_max": CANDIDATE_BYTES_MAX,
        "frame_bytes_max": crate::live_apps::FRAME_BYTES_MAX,
        "app_view_scratch_bytes_max": crate::live_apps::TRANSIENT_VIEW_BYTES_MAX,
        "aggregate_work_bytes_max": crate::live_apps::AGGREGATE_WORK_BYTES_MAX,
        "capture_transient_bytes_max": CAPTURE_TRANSIENT_BYTES_MAX})
    );
}

#[test]
fn rejects_identity_availability_counts_before_publication_and_accepts_the_retry() {
    // Goal: typed capture preserves the staging check that refusal counts match identity rows.
    // Method: corrupt one real decoded candidate's count, then reread its intact ring tick.
    let mut fixture = identity_fixture::Fixture::new();
    fixture.publish(1000);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let accepted = session.follow_latest().unwrap();
    fixture.publish(1001);
    assert!(matches!(
        session.sources[0].read_tick().unwrap(),
        Found::Publication { .. }
    ));
    session.sources[0].tick.processes_refused += 1;

    assert!(session.poll_publication(0, 2).is_err());
    assert_eq!(session.follow_latest().unwrap(), accepted);
    assert!(session.poll().unwrap());
    assert_eq!(
        session.follow_latest().unwrap()["selected"]["publication"],
        2
    );
}
