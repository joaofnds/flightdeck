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
