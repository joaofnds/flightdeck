use super::SealedRecording;
use serde_json::json;

#[test]
fn reads_a_partial_cpu_page_from_real_saved_counters_at_the_exact_same_second_identity() {
    // Goal: worker paging and exact inspection use acquired counters rather than aggregate CPU.
    // Method: encode 35 independent CPU counters and select the second same-second publication.
    let recording = SealedRecording::with_cpu_history();
    let mut request = SealedRecording::request();
    request.route = "cpu".into();
    request.second = Some(1001);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":3}));
    request.cpu_offset = 32;
    request.offset = 512;

    let snapshot = recording.read(request).unwrap();

    let history = snapshot.cpu_history.as_ref().unwrap();
    assert_eq!(
        history.target(history.selected().unwrap()).unwrap().1["publication"],
        3
    );
    assert_eq!(history.offset(), 32);
    assert_eq!(history.total(), 35);
    assert_eq!(
        history
            .cpus()
            .iter()
            .map(crate::live_cpu_history::CPUHistoryRow::cpu)
            .collect::<Vec<_>>(),
        vec![32, 33, 34]
    );
    assert_eq!(
        history.cpus()[0].busy_percent(),
        vec![None, Some(32.0), Some(32.0)]
    );
    assert_eq!(
        history.cpus()[2].busy_percent(),
        vec![None, Some(34.0), Some(34.0)]
    );
    assert_eq!(snapshot.selection["selected"]["publication"], 3);
    let activity = snapshot.cpu_detail.as_ref().unwrap();
    crate::live_source::identity_validate(&activity.observation, &snapshot.selection["selected"])
        .unwrap();
    assert_eq!(activity.cpus[32].busy_percent, Some(32.0));
    let crate::live_rankings::Rankings::Ready(rankings) = snapshot.rankings.as_deref().unwrap()
    else {
        panic!("Saved CPU App rankings are unavailable");
    };
    assert_eq!(rankings.selected["publication"], 3);
    assert!(snapshot.pages.is_empty());
}

#[test]
fn resets_a_cpu_page_beyond_a_smaller_domain_without_changing_the_observation() {
    // Goal: an old page offset cannot make a new recording appear unavailable.
    // Method: request CPU992 from an encoded35-CPU recording at its selected publication.
    let recording = SealedRecording::with_cpu_history();
    let mut request = SealedRecording::request();
    request.route = "cpu".into();
    request.second = Some(1001);
    request.cpu_offset = 992;

    let snapshot = recording.read(request).unwrap();

    assert_eq!(snapshot.cpu_history.as_ref().unwrap().offset(), 0);
    assert_eq!(snapshot.cpu_history.as_ref().unwrap().cpus().len(), 32);
    assert_eq!(snapshot.selection["selected"]["publication"], 2);
}

#[test]
fn leaves_cpu_history_absent_when_the_selected_second_has_no_observation() {
    // Goal: the CPU route cannot retain a measured heatmap across an explicit gap selection.
    // Method: read an unrecorded second from the real sealed-only fixture.
    let recording = SealedRecording::with_cpu_history();
    let mut request = SealedRecording::request();
    request.route = "cpu".into();
    request.second = Some(1002);

    let snapshot = recording.read(request).unwrap();

    assert!(snapshot.cpu_history.is_none());
    assert!(snapshot.cpu_detail.is_none());
    assert!(snapshot.selection["selected"].is_null());
}

impl SealedRecording {
    pub(super) fn with_cpu_history() -> Self {
        let token =
            collector::recording::identity_text(collector::recording::random_identity().unwrap());
        let recording = Self {
            root: std::env::temp_dir().join(format!("flightdeck-cpu-history-{token}")),
        };
        let store = persistence::Store::open(&recording.root).unwrap();
        let identity = persistence::WindowIdentity {
            run: collector::ring::RunIdentity {
                recording: store.recording(),
                run: collector::ring::Identity([1, 2]),
                boot: collector::ring::Identity([3, 4]),
            },
            window: 0,
        };
        let mut shape = Self::shape();
        shape.cpu_capacity = 35;
        let mut window =
            persistence::Window::new(identity, persistence::Limits::from_shape(&shape).unwrap());
        Self::append_cpu_history(
            &mut window,
            &shape,
            &CPUHistoryTick {
                publication: 1,
                wall_ns: 1_000_000_000_000,
            },
        );
        Self::append_cpu_history(
            &mut window,
            &shape,
            &CPUHistoryTick {
                publication: 2,
                wall_ns: 1_001_100_000_000,
            },
        );
        Self::append_cpu_history(
            &mut window,
            &shape,
            &CPUHistoryTick {
                publication: 3,
                wall_ns: 1_001_400_000_000,
            },
        );
        Self::commit(&store, identity, &window);
        drop(store);

        recording
    }

    fn append_cpu_history(
        window: &mut persistence::Window,
        shape: &collector::ring::Shape,
        observed: &CPUHistoryTick,
    ) {
        let mut tick = persistence::tick_buffer(shape).unwrap();
        tick.run_tick = observed.publication - 1;
        for cpu in 0..35_u32 {
            let publication = u32::try_from(observed.publication).unwrap();
            tick.cpus.push(collector::machine::CPUTicks {
                user: cpu * publication,
                system: 0,
                nice: 0,
                idle: (100 - cpu) * publication,
            });
        }
        window
            .push_tick(
                &persistence::Observation {
                    publication: observed.publication,
                    wall_ns: Some(observed.wall_ns),
                    continuous_ns: Some(observed.publication * 1_000_000_000),
                    awake_ns: Some(observed.publication * 1_000_000_000),
                },
                &tick,
                &[],
            )
            .unwrap();
    }
}

struct CPUHistoryTick {
    publication: u64,
    wall_ns: u64,
}
