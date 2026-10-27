use collector::ring::IdentityPublication;

#[test]
fn rehydrates_adjacent_event_only_app_identity() {
    // Goal: selecting an evicted raw sample preserves the adjacent captured App identity.
    // Method: retain both ring publications and scalar points, then discard raw cached rows.
    let mut fixture = identity_fixture::Fixture::new();
    publish(&mut fixture, 1);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish(&mut fixture, 2);
    session.poll().unwrap();
    session.follow_latest().unwrap();
    let kind = KeyKind::Bundle;
    let key = b"/Applications/B.app";
    let expected = session.app_detail(kind, key, 0).unwrap();
    session.observations.clear();
    session.cache_bytes = 0;
    let cursor = session.sources[0].reader.next_publication();
    let event_cursor = session.sources[0].reader.next_event();
    let losses = session.catalog()["reader_losses"].clone();

    session.select(1002, 0).unwrap();
    let detail = session.app_detail(kind, key, 0).unwrap();

    assert_eq!(session.apps_page(0).unwrap()["process_count"], 1);
    assert_eq!(detail, expected);
    assert_eq!(detail["app"]["members"], 1);
    assert_eq!(detail["app"]["live_members"], 0);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "0");
    assert_eq!(detail["app"]["cpu_percent_one_core"], Value::Null);
    assert_eq!(detail["app"]["energy_joules"], Value::Null);
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["known"],
        "104857600"
    );
    assert_eq!(session.sources[0].reader.next_publication(), cursor);
    assert_eq!(session.sources[0].reader.next_event(), event_cursor);
    assert_eq!(session.catalog()["reader_losses"], losses);
    assert!(!session.poll().unwrap());
}

fn publish(fixture: &mut identity_fixture::Fixture, publication: u64) {
    use collector::ring::{
        IdentityUnavailable, ProcessCreation, ProcessIdentity, ProgramBytes, UsageAvailability,
    };
    let creation = ProcessCreation {
        unique_id: 700,
        pid_version: 2,
    };
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &ProgramBytes {
                executable_name: b"worker",
                app_path: Some(b"/Applications/B.app"),
            },
            &mut program,
        )
        .unwrap();
    let event = if publication == 1 {
        collector::tracker::Event::Exec { pid: 7 }
    } else {
        collector::tracker::Event::Exit {
            pid: 7,
            last: Ok(publish_usage()),
            noticed: collector::tracker::Noticed::OnExit,
        }
    };
    let identity = IdentityObservation {
        pid: 7,
        usage: if publication == 1 {
            UsageAvailability::NotApplicable
        } else {
            UsageAvailability::Measured
        },
        identity: if publication == 1 {
            ProcessIdentity::Measured {
                creation,
                program: program.unwrap(),
            }
        } else {
            ProcessIdentity::Unavailable {
                creation: Some(creation),
                reason: IdentityUnavailable::Unsupported,
            }
        },
    };
    publish_observation(fixture, publication, &event, &identity);
}

fn publish_observation(
    fixture: &mut identity_fixture::Fixture,
    publication: u64,
    event: &collector::tracker::Event,
    identity: &IdentityObservation,
) {
    fixture
        .writer
        .publish_with_identity(
            &Readings {
                run_tick: publication,
                cpus: &[collector::machine::CPUTicks {
                    user: 1,
                    system: 0,
                    nice: 0,
                    idle: 1,
                }],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: std::slice::from_ref(event),
                dropped: Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: (1000 + publication) * 1_000_000_000,
                continuous_ns: publication * 1_000_000_000,
                awake_ns: publication * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &[],
                events: std::slice::from_ref(identity),
            },
        )
        .unwrap();
}

fn publish_usage() -> collector::processes::Usage {
    collector::processes::Usage {
        awake_ns_at_start: 0,
        user_ns: 400_000_000,
        system_ns: 0,
        energy_nj: 2_000_000_000,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 104_857_600,
        footprint_bytes: 110_000_000,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
    }
}

#[test]
fn keeps_verified_exit_facts_without_rates_across_a_run_tick_gap() {
    // Goal: a skipped scheduled tick withholds interval values but preserves exact exit facts.
    // Method: read a real live pair and its two sealed minutes through existing callers.
    let mut last = publish_usage();
    last.user_ns += 600_000_000;
    last.resident_bytes = 125_829_120;
    last.footprint_bytes = 131_072_000;
    let mut observed = std::array::from_fn(|_| Value::Null);
    gap_exit_outputs(&Ok(last), &mut observed);

    assert_eq!(observed[0]["detail"]["elapsed_ns"], Value::Null);
    gap_exit_assert(&observed[0], "exited_verified", &["125829120", "131072000"]);
    assert_eq!(observed[0], observed[1]);
    assert_eq!(observed[0], observed[2]);
    gap_exit_assert_loading(&observed[3]);
}

#[test]
fn restores_unverified_exit_last_gauges_across_a_saved_run_tick_gap() {
    // Goal: lazy saved loading keeps supported identity and last gauges without an interval.
    // Method: use the real prior window, then remove only that window's marker on a warm hit.
    let mut observed = std::array::from_fn(|_| Value::Null);
    gap_exit_outputs(
        &Err(collector::processes::Unreadable::Refused),
        &mut observed,
    );

    assert_eq!(observed[1]["apps"]["app_count"], 1);
    gap_exit_assert(
        &observed[1],
        "exited_unverified",
        &["104857600", "110000000"],
    );
    assert_eq!(observed[1], observed[0]);
    assert_eq!(observed[1], observed[2]);
    gap_exit_assert_loading(&observed[3]);
}

fn gap_exit_outputs(
    last: &std::result::Result<collector::processes::Usage, collector::processes::Unreadable>,
    output: &mut [Value; 4],
) {
    use collector::ring::{
        IdentityUnavailable, ProcessCreation, ProcessIdentity, UsageAvailability,
    };
    let mut fixture = identity_fixture::Fixture::new();
    fixture.start_at_run_tick(58);
    fixture.publish_with_usage(1058, &publish_usage());
    let mut live = LiveSession::open(&fixture.root).unwrap();
    assert_eq!(live.follow_latest().unwrap()["selected"]["publication"], 1);
    let event = collector::tracker::Event::Exit {
        pid: 7,
        last: *last,
        noticed: collector::tracker::Noticed::AlreadyExited,
    };
    let identity = IdentityObservation {
        pid: 7,
        usage: if last.is_ok() {
            UsageAvailability::Measured
        } else {
            UsageAvailability::Refused
        },
        identity: ProcessIdentity::Unavailable {
            creation: Some(ProcessCreation {
                unique_id: 7,
                pid_version: -7,
            }),
            reason: IdentityUnavailable::Unsupported,
        },
    };
    publish_observation(&mut fixture, 60, &event, &identity);
    assert!(live.poll().unwrap());
    let selected = live.follow_latest().unwrap();
    let run = fixture.writer.identity().unwrap();
    assert_eq!(
        selected["selected"]["run"],
        identity_text(run.run).to_string()
    );
    assert_eq!(selected["selected"]["publication"], 2);
    assert_eq!(selected["selected"]["run_tick"], 60);
    output[0] = json!({"apps": live.apps_page(0).unwrap(),
        "detail": live.app_detail(KeyKind::Bundle, b"/Apps/Current.app", 0).unwrap()});
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    gap_exit_saved(&fixture, output);
    println!("{}", json!({"gap_exit_outputs": output}));
    drop(live);
}

fn gap_exit_saved(fixture: &identity_fixture::Fixture, output: &mut [Value; 4]) {
    let mut saved = Session::open(&fixture.root).unwrap();
    let selected = saved.select(1060, 0).unwrap()["selected"].clone();
    let reads = saved.source_reads.get();
    output[1] = saved.with_app_sample(&selected, gap_exit_sample).unwrap();
    let cold_reads = saved.source_reads.get() - reads;
    let reads = saved.source_reads.get();
    let repeated = saved.with_app_sample(&selected, gap_exit_sample).unwrap();
    let warm_reads = saved.source_reads.get() - reads;
    gap_exit_sealed(fixture, &saved, &mut output[2]);
    gap_exit_histories(&saved, &selected, &output[1]);
    let prior =
        persistence::layout::WindowKey::from(&saved.windows[0].identity).marker(&fixture.root);
    let current =
        persistence::layout::WindowKey::from(&saved.windows[1].identity).marker(&fixture.root);
    assert!(!std::fs::read(&prior).unwrap().is_empty());
    std::fs::remove_file(prior).unwrap();
    let refused = saved.with_app_sample(&selected, gap_exit_sample);
    output[3] = json!({"cold_reads": cold_reads, "warm_reads": warm_reads,
        "repeat_equal": repeated == output[1], "current_marker_exists": current.exists(),
        "missing_prior_refused": matches!(refused, Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound)});
    drop(saved);
}

fn gap_exit_histories(saved: &Session, selection: &Value, expected: &Value) {
    use crate::live_app_history::{History, Target};
    use crate::live_app_response::Selected;
    let mut selected = None;
    saved
        .with_app_sample(selection, |sample| {
            selected = Some(Selected::ending_at(sample.selected)?);
            Ok(Value::Null)
        })
        .unwrap();
    let selected = selected.unwrap();
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    let history = saved
        .with_app_history(&selected, &target, |points| {
            Ok(json!(
                points
                    .iter()
                    .map(|point| point.json(0, false))
                    .collect::<Vec<_>>()
            ))
        })
        .unwrap();
    let point = history.as_array().unwrap().last().unwrap();
    assert_eq!(point["observation"], expected["detail"]["selected"]);
    assert_eq!(point["members"], 1);
    assert_eq!(point["elapsed_ns"], Value::Null);
    assert_eq!(point["cpu_ns"], expected["detail"]["app"]["cpu_ns"]);
    assert_eq!(
        point["resident_bytes"],
        expected["detail"]["app"]["resident_bytes"]
    );
    let mut attribution = Value::Null;
    saved
        .visit_app_attribution(
            &selected,
            |_| Ok(false),
            |frame, previous, _windows| {
                if frame.stamp().publication == 2 {
                    let mut sample = None;
                    Sample::capture(frame, previous, &mut sample)?;
                    attribution = gap_exit_sample(&sample.unwrap())?;
                }
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(attribution, *expected);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let groups = History::new()
        .process_groups_response(&selected, &target, Some(saved), &[], &operation)
        .unwrap();
    assert_eq!(groups.named_group_count, 1);
    let point = groups.points.last().unwrap();
    assert_eq!(point.observation, *selected.observation());
    assert_eq!(point.elapsed_ns, None);
    assert_eq!(point.readings[0].members, 1);
    assert_eq!(point.readings[0].measures[0].measured_members, 0);
    assert_eq!(point.readings[0].measures[0].unavailable_members, 1);
}

fn gap_exit_sample(sample: &Sample) -> Result<Value> {
    let page = Page {
        offset: 0,
        limit: 512,
    };
    let apps = sample.apps_page(page)?;
    let detail = if sample.apps().is_empty() {
        Value::Null
    } else {
        sample.app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page,
        })?
    };
    Ok(json!({"apps": apps, "detail": detail}))
}

fn gap_exit_sealed(fixture: &identity_fixture::Fixture, saved: &Session, output: &mut Value) {
    let mut before = None;
    let prior =
        persistence::layout::WindowKey::from(&saved.windows[0].identity).marker(&fixture.root);
    let current =
        persistence::layout::WindowKey::from(&saved.windows[1].identity).marker(&fixture.root);
    let prior = crate::recording::SealedMinute::open(&fixture.root, &prior).unwrap();
    prior
        .visit_samples(&mut before, |_, sample| sample.map(|_| ()))
        .unwrap();
    let current = crate::recording::SealedMinute::open(&fixture.root, &current).unwrap();
    current
        .visit_samples(&mut before, |endpoint, sample| {
            assert_eq!(endpoint.stamp.publication, 2);
            *output = gap_exit_sample(sample?)?;
            Ok(())
        })
        .unwrap();
}

fn gap_exit_assert(observed: &Value, state: &str, gauges: &[&str; 2]) {
    let apps = &observed["apps"];
    let detail = &observed["detail"];
    assert_eq!(apps["app_count"], 1);
    assert_eq!(apps["process_count"], 1);
    assert_eq!(detail["selected"]["stamp"]["publication"], "2");
    assert_eq!(detail["selected"]["run_tick"], "60");
    assert_eq!(detail["selected"]["wall_ns"], "1060000000000");
    assert_eq!(detail["selected"]["stamp"]["continuous_ns"], "60000000000");
    assert_eq!(detail["selected"]["stamp"]["awake_ns"], "60000000000");
    let mut previous = detail["selected"]["stamp"].clone();
    previous["publication"] = json!("1");
    previous["continuous_ns"] = json!("58000000000");
    previous["awake_ns"] = json!("58000000000");
    assert_eq!(detail["previous"], previous);
    assert_eq!(detail["elapsed_ns"], Value::Null);
    assert_eq!(detail["membership_complete"], false);
    assert_eq!(detail["ungrouped_members"], 0);
    assert_eq!(detail["usage_refused"], 0);
    assert_eq!(detail["usage_gone"], 0);
    assert_eq!(detail["app_count"], 1);
    assert_eq!(detail["process_count"], 1);
    assert_eq!(
        detail["page"],
        json!({"offset": 0, "limit": 512, "total": 1, "next_offset": null})
    );
    assert_eq!(detail["app"], gap_exit_app());
    assert_eq!(
        detail["processes"],
        json!([gap_exit_process(state, gauges)])
    );
}

fn gap_exit_app() -> Value {
    let interval = json!({"known": "0", "measured_members": 0,
        "unavailable_members": 1, "overflowed": false});
    let empty = json!({"known": "0", "measured_members": 0,
        "unavailable_members": 0, "overflowed": false});
    json!({"app_index": 0, "display_name": "Current.app",
        "key": {"kind": "bundle", "bytes_hex": "2f417070732f43757272656e742e617070",
            "display": "/Apps/Current.app"},
        "members": 1, "live_members": 0, "membership_complete": false,
        "cpu_ns": interval.clone(), "energy_nj": interval.clone(), "gpu_ns": interval.clone(),
        "disk_read_bytes": interval.clone(), "disk_written_bytes": interval.clone(),
        "disk_io_bytes": interval, "disk_read_bps": null, "disk_written_bps": null,
        "disk_io_bps": null, "resident_bytes": empty.clone(), "footprint_bytes": empty,
        "cpu_percent_one_core": null, "energy_joules": null, "energy_watts": null,
        "gpu_percent": null})
}

fn gap_exit_process(state: &str, gauges: &[&str; 2]) -> Value {
    let interval = json!({"known": null, "unavailable": "interval"});
    json!({"pid": 7, "creation": {"unique_id": "7", "pid_version": -7},
        "app_index": 0, "executable_name": {"bytes_hex": "63757272656e74", "display": "current"},
        "program_unavailable": null, "cpu_ns": interval.clone(), "energy_nj": interval.clone(),
        "gpu_ns": interval.clone(), "disk_read_bytes": interval.clone(),
        "disk_written_bytes": interval.clone(), "disk_io_bytes": interval.clone(),
        "disk_read_bps": null, "disk_written_bps": null, "disk_io_bps": null,
        "resident_bytes": {"known": gauges[0], "unavailable": null},
        "footprint_bytes": {"known": gauges[1], "unavailable": null},
        "idle_wakeups": interval.clone(), "pageins": interval,
        "threads": {"known": null, "unavailable": "unknown"},
        "idle_wakeups_per_second": null, "pageins_per_second": null,
        "app_attribution": null, "state": state, "cpu_percent_one_core": null,
        "energy_joules": null, "energy_watts": null, "gpu_percent": null})
}

fn gap_exit_assert_loading(observed: &Value) {
    assert_eq!(observed["cold_reads"], 6);
    assert_eq!(observed["warm_reads"], 0);
    assert_eq!(observed["repeat_equal"], true);
    assert_eq!(observed["current_marker_exists"], true);
    assert_eq!(observed["missing_prior_refused"], true);
}

#[test]
fn restores_rates_from_the_first_consecutive_successor_after_a_gap() {
    // Goal: the next consecutive observation uses the gap observation, never an older baseline.
    // Method: compare complete live and saved App pages from native run ticks zero, two, three.
    let mut fixture = identity_fixture::Fixture::new();
    fixture.publish_with_system_rows(1001);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.follow_latest().unwrap();
    fixture.skip_run_ticks(1);
    fixture.publish_with_system_rows(1003);
    assert!(live.poll().unwrap());
    live.follow_latest().unwrap();
    let gap = live.apps_page(0).unwrap();
    fixture.publish_with_system_rows(1004);
    assert!(live.poll().unwrap());
    live.follow_latest().unwrap();
    let successor = live.apps_page(0).unwrap();
    assert_eq!(gap["elapsed_ns"], Value::Null);
    assert_eq!(gap["previous"]["publication"], "1");
    assert_eq!(gap["apps"][0]["cpu_ns"]["measured_members"], 0);
    assert_eq!(successor["previous"]["publication"], "2");
    assert_eq!(successor["previous"]["continuous_ns"], "2000000000");
    assert_eq!(successor["elapsed_ns"], "1000000000");
    assert_eq!(successor["apps"][0]["cpu_ns"]["known"], "1");
    let rate = successor["apps"][0]["cpu_percent_one_core"]
        .as_f64()
        .unwrap();
    assert!((rate - 0.000_000_1).abs() < 1e-20);
    fixture.seal();
    let mut saved = Session::open(&fixture.root).unwrap();
    let selected = saved.select(1003, 0).unwrap()["selected"].clone();
    let saved_gap = saved
        .with_app_sample(&selected, |sample| {
            sample.apps_page(Page {
                offset: 0,
                limit: 512,
            })
        })
        .unwrap();
    let selected = saved.select(1004, 0).unwrap()["selected"].clone();
    let saved_successor = saved
        .with_app_sample(&selected, |sample| {
            sample.apps_page(Page {
                offset: 0,
                limit: 512,
            })
        })
        .unwrap();
    assert_eq!(saved_gap, gap);
    assert_eq!(saved_successor, successor);
}
