#[test]
fn restores_independent_cpu_history_on_cold_open_and_reopen() {
    // Goal: committed CPU intervals join the retained ring without aggregate replication.
    // Method: capture 901 real 18-CPU readings, seal14 minutes, then cold-open twice.
    let mut fixture = RingFixture::new_following();
    history_publish_committed(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let selection = session.follow_latest().unwrap();
    let cursor = session.sources[0].reader.next_publication();
    let losses = session.sources[0].losses;

    let history = session.cpu_history(0).unwrap();

    history_assert_full(&history, &selection);
    let reads = session.history.as_ref().unwrap().source_reads.get();
    assert_eq!(session.cpu_history(0).unwrap(), history);
    assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
    assert_eq!(session.sources[0].reader.next_publication(), cursor);
    assert_eq!(session.sources[0].losses, losses);
    drop(session);
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    assert_eq!(reopened.follow_latest().unwrap(), selection);
    let data = fixture.root.join("data");
    let held = fixture.root.join("held-data");
    std::fs::rename(&data, &held).unwrap();
    let degraded = reopened.cpu_history(0).unwrap();
    assert_eq!(degraded["observation"], selection["selected"]);
    assert_eq!(degraded["observations"].as_array().unwrap().len(), 900);
    assert_eq!(degraded["retention_limited"], true);
    assert_eq!(degraded["cpus"][0]["busy_percent"][0], Value::Null);
    assert_eq!(degraded["cpus"][0]["busy_percent"][899], 0.0);
    assert!(degraded["restoration_error"].is_string());
    assert!(reopened.catalog()["history_error"].is_null());
    assert_eq!(
        reopened.page("memory", 0).unwrap()["observation"],
        selection["selected"]
    );
    std::fs::rename(&held, &data).unwrap();
    assert_eq!(reopened.cpu_history(0).unwrap(), history);
    history_assert_saved(&fixture.root);
}

#[test]
fn reads_cpu_history_over_a_context_that_holds_later_seconds() {
    // Goal: CPU history of a selected second covers the seconds its context holds after it.
    // Method: publish seconds 1001 to 1901 and seal 1001 to 1840, select 1400 live and saved,
    // then read the range and the readings each CPU history covers.
    let mut fixture = RingFixture::new_following();
    history_publish_committed(&mut fixture);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let mut saved = Session::open(&fixture.root).unwrap();
    live.select(1400, 0).unwrap();
    saved.select(1400, 0).unwrap();

    let live_history = live.cpu_history(0).unwrap();
    let saved_history = saved.cpu_history(0).unwrap();

    assert_eq!(live_history["observation"]["publication"], 400);
    assert_eq!(live_history["first_ns"], 951 * NANOSECONDS_PER_SECOND);
    assert_eq!(live_history["end_ns"], 1851 * NANOSECONDS_PER_SECOND);
    assert_eq!(live_history["observations"].as_array().unwrap().len(), 850);
    assert_eq!(saved_history["observation"]["publication"], 400);
    assert_eq!(saved_history["end_ns"], 1841 * NANOSECONDS_PER_SECOND);
    assert_eq!(saved_history["observations"].as_array().unwrap().len(), 840);
}

fn history_assert_saved(root: &Path) {
    let mut saved = Session::open(root).unwrap();
    let selected = saved.select(1840, 0).unwrap();
    let history = saved.cpu_history(0).unwrap();
    assert_eq!(history["observation"], selected["selected"]);
    assert_eq!(history["observation"]["publication"], 840);
    assert_eq!(history["total"], 18);
    assert_eq!(history["observations"].as_array().unwrap().len(), 840);
    assert_eq!(history["observations"][0]["interval_available"], false);
    assert_eq!(
        history["observations"][1]["previous_observation"]["publication"],
        1
    );
    let detail = saved.cpu_detail().unwrap();
    for cpu in 0..18 {
        assert_eq!(history["cpus"][cpu]["busy_percent"][0], Value::Null);
        assert_eq!(
            history["cpus"][cpu]["busy_percent"][839],
            detail["cpus"][cpu]["busy_percent"]
        );
    }
    let reads = saved.source_reads.get();
    assert_eq!(saved.cpu_history(0).unwrap(), history);
    assert_eq!(saved.source_reads.get(), reads);
    assert_eq!(
        saved.page("memory", 0).unwrap()["observation"],
        selected["selected"]
    );
}

#[test]
fn retains_previously_observed_cpu_history_after_real_raw_byte_eviction_before_sealing() {
    // Goal: entering CPU after other panes retains observed intervals despite raw byte eviction.
    // Method: follow901 unsealed readings, with2048 genuine processes in the final202 ticks.
    let mut fixture = RingFixture::new_with_shape(&Shape {
        slot_count: 120,
        cpu_capacity: 18,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 2048,
        coalition_capacity: 1,
        event_capacity: 8,
    });
    history_publish(&mut fixture, 1, false);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    session.page("memory", 0).unwrap();
    for publication in 2..=901 {
        history_publish(&mut fixture, publication, publication >= 700);
        assert!(session.poll().unwrap());
    }
    let selection = session.follow_latest().unwrap();
    let cursor = session.sources[0].reader.next_publication();
    let losses = session.sources[0].losses;
    assert!(
        session
            .observations
            .front()
            .unwrap()
            .observation
            .publication
            > 2
    );
    assert!(session.cache_bytes <= CACHE_BYTES_MAX);
    println!(
        "{}",
        json!({"phase":"raw_byte_eviction","raw_count":session.observations.len(),
        "raw_first":session.observations.front().unwrap().observation.publication,
        "raw_bytes":session.cache_bytes,"scalar_count":session.points.len()})
    );

    let history = session.cpu_history(0).unwrap();

    history_assert_full(&history, &selection);
    println!(
        "{}",
        json!({"phase":"live_cpu_retention","logical_cpus":18,"points":900,
        "accounted_bytes":session.cpu_history.as_ref().unwrap().bytes()})
    );
    assert_eq!(session.sources[0].reader.next_publication(), cursor);
    assert_eq!(session.sources[0].losses, losses);
    assert_eq!(session.selected.as_ref(), Some(&selection["selected"]));
}

fn history_assert_full(history: &Value, selection: &Value) {
    assert_eq!(history["observation"], selection["selected"]);
    assert_eq!(history["context_seconds"], 900);
    assert_eq!(history["first_ns"], 1002 * NANOSECONDS_PER_SECOND);
    assert_eq!(history["end_ns"], 1902 * NANOSECONDS_PER_SECOND);
    assert_eq!(history["total"], 18);
    assert_eq!(history["offset"], 0);
    assert_eq!(history["retention_limited"], false);
    let observations = history["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 900);
    assert_eq!(history["cpus"].as_array().unwrap().len(), 18);
    for (index, point) in observations.iter().enumerate() {
        let publication = u64::try_from(index).unwrap() + 2;
        assert_eq!(point["observation"]["publication"], publication);
        assert_eq!(point["observation"]["run_tick"], publication);
        assert_eq!(
            point["observation"]["wall_ns"],
            (1000 + publication) * NANOSECONDS_PER_SECOND
        );
        assert_eq!(
            point["previous_observation"]["publication"],
            publication - 1
        );
        assert_eq!(point["interval_available"], true);
        assert_eq!(point["cpu_count"], 18);
    }
    for cpu in 0..18 {
        let expected = if cpu == 17 {
            100.0
        } else {
            f64::from(u32::try_from(cpu).unwrap()) * 5.0
        };
        assert_eq!(history["cpus"][cpu]["cpu"], cpu);
        let values = history["cpus"][cpu]["busy_percent"].as_array().unwrap();
        assert_eq!(values.len(), 900);
        for value in values {
            assert!((value.as_f64().unwrap() - expected).abs() < 1e-10);
        }
    }
}

fn history_publish_committed(fixture: &mut RingFixture) {
    let mut missing = None;
    history_publish_withheld(fixture, None, &mut missing);
}

fn history_publish(fixture: &mut RingFixture, tick: u32, process_pressure: bool) {
    let cpus = std::array::from_fn::<_, 18, _>(|cpu| {
        let rate = if cpu == 17 {
            100
        } else {
            u32::try_from(cpu).unwrap() * 5
        };
        collector::machine::CPUTicks {
            user: tick * (rate / 2),
            system: 0,
            nice: tick * (rate - rate / 2),
            idle: tick * (100 - rate),
        }
    });
    let processes = if process_pressure {
        history_processes(tick)
    } else {
        Vec::new()
    };
    fixture.writer.publish_observed(
        &Readings {
            run_tick: u64::from(tick),
            cpus: &cpus,
            memory: &collector::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &processes,
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        },
        &ObservationTimes {
            wall_ns: (1000 + u64::from(tick)) * NANOSECONDS_PER_SECOND,
            continuous_ns: u64::from(tick - 1) * NANOSECONDS_PER_SECOND,
            awake_ns: u64::from(tick - 1) * NANOSECONDS_PER_SECOND,
        },
    );
}

fn history_processes(tick: u32) -> Vec<collector::processes::Process> {
    use collector::processes::{Process, Task, Usage};
    let counter = 1_000_000_000_000 + u64::from(tick);
    (1..=2048)
        .map(|pid| Process {
            pid,
            usage: Usage {
                awake_ns_at_start: 0,
                user_ns: counter,
                system_ns: counter,
                energy_nj: counter,
                instructions: counter,
                super_instructions: counter,
                cycles: counter,
                idle_wakeups: counter,
                interrupt_wakeups: counter,
                pageins: counter,
                resident_bytes: counter,
                footprint_bytes: counter,
                disk_read_bytes: counter,
                disk_written_bytes: counter,
            },
            task: Task {
                threads: 1,
                faults: 1,
                context_switches: 1,
                mach_syscalls: 1,
                unix_syscalls: 1,
            },
        })
        .collect()
}

#[test]
fn restores_cpu_intervals_from_an_older_late_commit_without_advancing_selection() {
    // Goal: CPU history repairs an actual old gap even when no new live publication arrives.
    // Method: withhold minute12 outside the120-slot ring, then seal it and poll256 times.
    let mut fixture = RingFixture::new_following();
    let mut missing = None;
    history_publish_withheld(&mut fixture, Some(12), &mut missing);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let selection = session.follow_latest().unwrap();
    let initial = session.cpu_history(0).unwrap();
    assert_eq!(initial["observations"].as_array().unwrap().len(), 840);
    let boundary = initial["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["observation"]["publication"] == 781)
        .unwrap();
    assert_eq!(boundary["interval_available"], false);
    let cursor = session.sources[0].reader.next_publication();
    let losses = session.sources[0].losses;
    seal_window(&fixture.root, missing.as_ref().unwrap());

    for _poll in 0..256 {
        assert!(!session.poll().unwrap());
    }
    let recovered_selection = session.follow_latest().unwrap();
    let recovered = session.cpu_history(0).unwrap();

    assert_eq!(recovered_selection["selected"], selection["selected"]);
    history_assert_full(&recovered, &recovered_selection);
    assert_eq!(session.sources[0].reader.next_publication(), cursor);
    assert_eq!(session.sources[0].losses, losses);
    let reads = session.history.as_ref().unwrap().source_reads.get();
    assert_eq!(session.cpu_history(0).unwrap(), recovered);
    assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
}

#[test]
fn refuses_canceled_cpu_restoration_without_installing_its_cache_or_changing_selection() {
    // Goal: cancellation cannot install a complete-looking CPU cache or move live selection.
    // Method: cancel after genuine window queries, then retry through a new public generation.
    let mut fixture = RingFixture::new_following();
    history_publish_committed(&mut fixture);
    let control = Arc::new(Cancellation::default());
    let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
    let selection = session.follow_latest().unwrap();
    let cursor = session.sources[0].reader.next_publication();
    let losses = session.sources[0].losses;
    let cancel = control.clone();
    session.history.as_mut().unwrap().before_cpu_history_install = Some(Box::new(move || {
        cancel.cancel().unwrap();
    }));

    let result = session.cpu_history(0);

    assert!(matches!(result,Err(Error::Io(error))
        if error.kind()==std::io::ErrorKind::Interrupted));
    assert!(
        session
            .history
            .as_ref()
            .unwrap()
            .cpu_history_cache
            .is_none()
    );
    assert_eq!(session.selected.as_ref(), Some(&selection["selected"]));
    assert_eq!(session.sources[0].reader.next_publication(), cursor);
    assert_eq!(session.sources[0].losses, losses);
    session.begin(control.epoch()).unwrap();
    let retried_selection = session.follow_latest().unwrap();
    let retried = session.cpu_history(0).unwrap();
    assert_eq!(retried_selection["selected"], selection["selected"]);
    history_assert_full(&retried, &retried_selection);
}

fn history_publish_withheld(
    fixture: &mut RingFixture,
    withheld_window: Option<u64>,
    missing: &mut Option<Window>,
) {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    for minute in 0..14 {
        let mut window = Window::new(
            WindowIdentity {
                run: reader.identity().unwrap(),
                window: minute,
            },
            Limits::from_shape(&reader.shape()).unwrap(),
        );
        for offset in 1..=60 {
            let publication = minute * 60 + offset;
            history_publish(fixture, u32::try_from(publication).unwrap(), false);
            follow_restoration_stage(&mut reader, &mut tick, &mut window, publication);
        }
        if Some(minute) == withheld_window {
            *missing = Some(window);
        } else {
            seal_window(&fixture.root, &window);
        }
    }
    for publication in 841..=901 {
        history_publish(fixture, publication, false);
    }
}

#[test]
fn measures_cpu_history_restoration_projection_and_serialization_separately() {
    // Goal: keep genuine18-CPU query/projection costs separate from extra bridge serialization.
    // Method: prepare901 publications, then measure cold restoration and20 unchanged requests.
    let mut fixture = RingFixture::new_following();
    history_publish_committed(&mut fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let selection = session.follow_latest().unwrap();
    let mut output = None;
    follow_restoration_measure("cold_cpu_history_with_bound_serialization", 0, || {
        output = Some(session.cpu_history(0).unwrap());
    });
    history_assert_full(output.as_ref().unwrap(), &selection);
    let reads = session.history.as_ref().unwrap().source_reads.get();
    for sample in 0..20 {
        follow_restoration_measure("warm_cpu_history_with_bound_serialization", sample, || {
            output = Some(session.cpu_history(0).unwrap());
        });
        follow_restoration_measure("cpu_history_extra_serialization", sample, || {
            let bytes = serde_json::to_vec(output.as_ref().unwrap()).unwrap();
            assert!(bytes.len() < 7 * 1024 * 1024);
            std::hint::black_box(bytes);
        });
    }

    assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
    let saved = session
        .history
        .as_ref()
        .unwrap()
        .cpu_history_cache
        .as_ref()
        .unwrap();
    println!(
        "{}",
        json!({"phase":"cpu_history_accounting","logical_cpus":18,
        "context_observations":900,"saved_observations":saved.entries.len(),
        "saved_accounted_bytes":saved.bytes(),
        "live_accounted_bytes":session.cpu_history.as_ref().unwrap().bytes(),
        "payload_bytes":serde_json::to_vec(output.as_ref().unwrap()).unwrap().len()})
    );
}

#[test]
fn reads_cpu_history_of_a_selected_second_whose_charts_reach_a_saved_lead_in() {
    // Goal: a selected second whose charts read a saved second before its context still shows
    // its CPU history, which covers only the context's 900 seconds.
    // Method: select a second whose charts reach a saved second before its context, then read
    // the range and the readings its CPU history covers.
    let fixture = window::fixture::saved_with_apps_after_a_lead_in();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1931, 0).unwrap();

    let history = session.cpu_history(0).unwrap();

    assert_eq!(history["first_ns"], 1091 * NANOSECONDS_PER_SECOND);
    assert_eq!(history["observations"].as_array().unwrap().len(), 3);
}
