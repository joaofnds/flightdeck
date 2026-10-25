use super::*;

#[test]
fn a_ranking_error_retains_only_a_bounded_notice() {
    // Goal: a large error from the viewer cannot enlarge the ranking failure the page keeps.
    // Method: hand the selected observation's ranks a 4096-character error.
    let result = from_selected(Err("失".repeat(4096)));

    let Rankings::Unavailable(error) = result else {
        panic!("{result:?}")
    };
    assert_eq!(error.chars().count(), 513);
    assert!(error.ends_with('…'));
}

#[test]
fn the_viewers_unranked_counts_reach_the_page() {
    // Goal: the Apps the viewer could not rank stay counted on the page, so an empty ranking
    // can say they were unavailable rather than idle.
    // Method: hand the page a GPU ranking of three Apps, two without GPU time and one whose sum
    // overflowed, and read its counts.
    let stamp = viewer::apps::Stamp {
        boot: [1, 2],
        run: [3, 4],
        publication: 5,
        continuous_ns: None,
        awake_ns: None,
    };
    let mut metrics = [
        viewer::live_app_response::Metric::Cpu,
        viewer::live_app_response::Metric::Resident,
        viewer::live_app_response::Metric::Power,
        viewer::live_app_response::Metric::Disk,
        viewer::live_app_response::Metric::Gpu,
    ]
    .map(|metric| viewer::live_app_response::Ranking {
        metric,
        entries: Vec::new(),
        unavailable: 0,
        overflowed: 0,
        partial: 0,
    });
    metrics[4].unavailable = 2;
    metrics[4].overflowed = 1;
    let ranks = Ranks {
        selected: viewer::live_app_response::Observation {
            recording: [6, 7],
            stamp,
            run_tick: 8,
            wall_ns: None,
        },
        app_count: 3,
        membership_complete: true,
        ungrouped_members: 0,
        metrics,
    };

    let top_apps = from_ranks(&ranks);

    let gpu = &top_apps.metrics[4];
    assert_eq!(gpu.metric, Metric::Gpu);
    assert_eq!((gpu.unavailable, gpu.overflowed), (2, 1));
    assert_eq!(top_apps.app_count, 3);
}

#[test]
fn a_sampled_ranking_preserves_each_metrics_distinct_amount_and_rate() {
    // Goal: sampled ranks keep their units, names and exact selected identity in presentation.
    // Method: retain five distinct positive measures, then compare with independently set rates.
    let sample = sampled_metrics("Measured");

    let rankings = from_sampled(&sample);

    let expected = fixture::one_bundle(
        &stamp_identity(&sample.observation.stamp),
        "Measured",
        "41",
        [
            (500_000_000, Some(25.0)),
            (1_024, None),
            (4_000_000_000, Some(2.0)),
            (600, Some(300.0)),
            (100_000_000, Some(5.0)),
        ],
    );
    assert_eq!(rankings, expected);
}

#[test]
fn sampled_rankings_refuse_names_beyond_the_existing_response_bound() {
    // Goal: a retained sample cannot bypass the existing App response's presentation bound.
    // Method: retain a valid name whose five displayed copies exceed that bound.
    let sample = sampled_metrics(&"X".repeat(viewer::live_app_response::RESPONSE_BYTES_MAX));
    assert!(sample.ranks().is_ok());

    let rankings = from_sampled(&sample);

    assert!(matches!(rankings, Rankings::Unavailable(_)));
}

fn sampled_metrics(name: &str) -> viewer::store::SampledSecond {
    use viewer::live_app_response::{App, Key, Measure, RankedApp};
    use viewer::store::{AppRow, ByteBudget, SampledSecond, Totals};
    let second = crate::live_store_fixtures::reading_at(8_200, 0);
    let measure = |known| Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let app = App {
        key: Key {
            kind: KeyKind::Bundle,
            bytes: vec![65],
            display: name.into(),
        },
        display_name: name.into(),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measure(500_000_000),
        energy_nj: measure(4_000_000_000),
        gpu_ns: measure(100_000_000),
        disk_read_bytes: measure(200),
        disk_written_bytes: measure(400),
        disk_io_bytes: measure(600),
        resident_bytes: measure(1_024),
        footprint_bytes: measure(2_048),
    };
    let name = Arc::new(RankedApp {
        kind: KeyKind::Bundle,
        bytes: vec![65],
        display_name: name.into(),
    });
    let mut apps = crate::live_store_app_fixtures::apps_at(&second, &[]);
    apps.apps = vec![AppRow::new(&app, name)];
    apps.totals = Totals::of(&apps.apps);
    apps.elapsed_ns = Some(2_000_000_000);
    SampledSecond::from_second(&second, &apps, &Arc::new(ByteBudget::new(536_870_912)))
}
