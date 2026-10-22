use super::{
    axis, cards, charts, minute_rows, nearest, report_rows, run_status, seal_rows, sources,
};
use crate::chart_axes::TimeWindow;
use crate::self_reports::{MinuteReport, Seal, SealTotals, SelfReports, TickReport};

const WINDOW: TimeWindow = TimeWindow {
    first: 1_000,
    span: 900,
};

fn tick(second: u64) -> TickReport {
    TickReport {
        tick_first: 1,
        tick_last: 10,
        second,
        collector_cpu_percent: 1.42,
        collector_milliwatts: 17.16,
        late_average_ms: 3.2,
        late_maximum_ms: 9.0,
        slips: 1,
        sources: vec![
            ("processes".into(), 4.63),
            ("ioreport".into(), 6.23),
            ("smc".into(), 0.13),
        ],
        processes_observed: 1_069,
        processes_refused: 354,
        processes_gone: 0,
        event_drops: 0,
    }
}

fn minute(second: u64) -> MinuteReport {
    MinuteReport {
        second,
        elapsed_ns: 60_000_000_000,
        collector_cpu_ns: 852_000_000,
        persister_cpu_ns: 148_200_000,
        combined_percent: 1.666,
        footprint_bytes: Some(10_863_247),
        footprint_bytes_max: None,
    }
}

fn started() -> SelfReports {
    let mut reports = SelfReports::default();
    reports.begin(1_000);
    reports
}

fn readings(reports: &SelfReports, inspected: Option<u64>) -> Vec<Vec<String>> {
    charts(reports, WINDOW, inspected)
        .iter()
        .map(|chart| {
            chart
                .legend
                .iter()
                .map(|legend| legend.reading.clone())
                .collect()
        })
        .collect()
}

#[test]
fn cards_show_the_last_minute_and_the_run_totals() {
    // Goal: the cards read the newest minute, every report's slips and the seal totals.
    // Method: fill a started run with two reports, a minute and seal totals.
    let mut reports = started();
    reports.ticks.extend([tick(1_010), tick(1_020)]);
    reports.minutes.push_back(minute(1_060));
    reports.seal_totals = SealTotals {
        windows: 6,
        partial: 1,
        publications_lost: 0,
        events_lost: 2,
    };

    let shown_cards = cards(&reports);
    let shown: Vec<_> = shown_cards
        .iter()
        .map(|card| (card.label.as_str(), card.reading.clone(), card.note.clone()))
        .collect();

    assert_eq!(
        shown,
        [
            (
                "Recording CPU",
                "1.67%".to_owned(),
                "Last complete minute · 5% of one core budget".to_owned()
            ),
            (
                "Scheduled tick slips",
                "2".to_owned(),
                "Across the 2 ten-second reports".to_owned()
            ),
            (
                "Sealed windows",
                "6".to_owned(),
                "5 full windows + 1 partial window".to_owned()
            ),
            (
                "Lost publications / events",
                "0 / 2".to_owned(),
                "Reported by seals; unknown-extent flag not captured".to_owned()
            ),
        ]
    );
}

#[test]
fn recording_cpu_waits_for_a_complete_minute() {
    // Goal: the budget card never shows a cost before a whole minute has been measured.
    // Method: read the cards of a started run that has reports but no minute.
    let mut reports = started();
    reports.ticks.push_back(tick(1_010));

    let recording = &cards(&reports)[0];

    assert_eq!(
        (recording.reading.as_str(), recording.note.as_str()),
        ("Unavailable", "No complete minute yet")
    );
}

#[test]
fn cards_say_not_captured_without_a_collector_this_app_started() {
    // Goal: an app that only reads a recording shows no made-up collection cost.
    // Method: read the cards of reports no run began.
    let shown: Vec<_> = cards(&SelfReports::default())
        .into_iter()
        .map(|card| (card.reading, card.note))
        .collect();

    assert_eq!(
        shown,
        vec![
            (
                "Not captured".to_owned(),
                "Collector not started by this app".to_owned()
            );
            4
        ]
    );
}

#[test]
fn legends_read_the_inspected_report_as_the_prototype_writes_them() {
    // Goal: each legend reads its value at the inspected second, in the chart's unit.
    // Method: inspect the second that holds both a report and a minute.
    let mut reports = started();
    reports.ticks.push_back(tick(1_060));
    reports.minutes.push_back(minute(1_060));

    let shown = readings(&reports, Some(1_060));

    assert_eq!(
        shown,
        [
            vec!["1.42 %", "0.25 %", "1.67 %"],
            vec!["1.42 %"],
            vec!["3.2 ms", "9.0 ms"],
            vec!["10.36 MiB", "Unavailable"],
            vec!["1,069 processes", "354 processes", "0 processes"],
            vec!["1 count", "0 count"],
        ]
    );
}

#[test]
fn a_chart_with_no_report_at_the_inspected_second_says_so() {
    // Goal: a minute chart never borrows a reading from another minute.
    // Method: inspect a report whose second closes no minute.
    let mut reports = started();
    reports.ticks.push_back(tick(1_010));
    reports.minutes.push_back(minute(1_060));

    let shown = readings(&reports, Some(1_010));

    assert_eq!(shown[0], ["No report", "No report", "No report"]);
}

#[test]
fn axes_split_the_scale_in_quarters_as_the_prototype_does() {
    // Goal: the budget chart's fixed scale and a count chart's whole scale label as drawn.
    // Method: read the axis of the budget chart and the process chart of one report.
    let mut reports = started();
    reports.ticks.push_back(TickReport {
        processes_observed: 1_084,
        ..tick(1_010)
    });

    let charts = charts(&reports, WINDOW, None);

    assert_eq!(
        axis(&charts[0])
            .into_iter()
            .map(|(_, label)| label)
            .collect::<Vec<_>>(),
        ["0", "1.4", "2.8", "4.1", "5.5"]
    );
    assert_eq!(
        axis(&charts[4])
            .into_iter()
            .map(|(_, label)| label)
            .collect::<Vec<_>>(),
        ["0", "312", "624", "936", "1,248"]
    );
}

#[test]
fn the_budget_line_stands_at_five_percent_of_the_budget_chart() {
    // Goal: the budget line marks 5% on the budget chart and on no other chart.
    // Method: read where each chart puts its budget line.
    let budgets: Vec<_> = charts(&started(), WINDOW, None)
        .iter()
        .map(|chart| chart.budget)
        .collect();

    assert_eq!(budgets, [Some(5.0 / 5.5), None, None, None, None, None]);
}

#[test]
fn the_pointer_inspects_the_nearest_report() {
    // Goal: hovering between reports picks the closer one.
    // Method: ask for the report nearest a second just past the middle of two reports.
    let mut reports = started();
    reports.ticks.extend([tick(1_010), tick(1_020)]);

    assert_eq!(nearest(&reports, 1_016), Some(1_020));
}

#[test]
fn sources_run_from_the_costliest_and_carry_the_prototype_names() {
    // Goal: the source bars read largest first, naming IOReport and SMC as the prototype does.
    // Method: sort the sources of one report.
    let shown = sources(&tick(1_010));

    assert_eq!(
        shown,
        [
            ("IOReport".to_owned(), 6.23),
            ("processes".to_owned(), 4.63),
            ("SMC".to_owned(), 0.13)
        ]
    );
}

#[test]
fn a_seal_shorter_than_a_minute_is_marked_partial() {
    // Goal: the seal table writes each window as the prototype does, partial ones marked.
    // Method: format a full seal and the short final one.
    let mut reports = started();
    let seal = Seal {
        window: 4,
        publication_first: 241,
        publication_end: 301,
        publications_lost: 0,
        events_lost: 0,
        sealing_ns: 147_200_000,
        staged_bytes: 12_694_061,
    };
    reports.seals.extend([
        seal.clone(),
        Seal {
            window: 5,
            publication_first: 301,
            publication_end: 302,
            ..seal
        },
    ]);

    let rows = seal_rows(&reports);

    assert_eq!(rows[0], ["4", "241…301", "147.2 ms", "12.11 MiB", "0", "0"]);
    assert_eq!(rows[1][0], "5 · partial");
}

#[test]
fn the_report_table_keeps_each_measure_in_its_digits() {
    // Goal: a report row reads as the prototype's captured report table does.
    // Method: format one report and compare every cell after its clock.
    let mut reports = started();
    reports.ticks.push_back(tick(1_010));

    let row = &report_rows(&reports)[0];

    assert_eq!(
        row[1..],
        [
            "1.42%", "17.16 mW", "3.2 ms", "9.0 ms", "1", "1069", "354", "0", "0"
        ]
    );
}

#[test]
fn a_table_row_ends_at_the_elapsed_time_of_its_report() {
    // Goal: a report and a minute end at the run's elapsed time, as the prototype's tables write
    // them. Method: format one of each, started at second 1,000, and read the first cell.
    let mut reports = started();
    reports.ticks.push_back(tick(1_010));
    reports.minutes.push_back(minute(1_060));

    assert_eq!(report_rows(&reports)[0][0], "0:10");
    assert_eq!(minute_rows(&reports)[0][0], "1:00");
}

#[test]
fn the_minute_table_splits_the_budget_between_collector_and_persister() {
    // Goal: a minute row reads each share of one core to three digits.
    // Method: format one minute and compare every cell after its clock.
    let mut reports = started();
    reports.minutes.push_back(minute(1_060));

    let row = &minute_rows(&reports)[0];

    assert_eq!(
        row[1..],
        ["1.420%", "0.247%", "1.666%", "10.36 MiB", "Unavailable"]
    );
}

#[test]
fn a_single_report_and_window_read_in_the_singular() {
    // Goal: a count of one reads as one thing, not as several.
    // Method: read the notes of a run with one report and one full sealed window.
    let mut reports = started();
    reports.ticks.push_back(tick(1_010));
    reports.seal_totals.windows = 1;

    let notes: Vec<_> = cards(&reports).into_iter().map(|card| card.note).collect();

    assert_eq!(
        notes[1..3],
        [
            "Across the 1 ten-second report",
            "1 full window + 0 partial windows"
        ]
    );
}

#[test]
fn the_run_status_says_the_reports_come_from_the_collector_this_app_started() {
    // Goal: the Self page does not call its reports a saved run with no live connection, since
    // the app reads them from the collector it started while that runs. Method: build the
    // status line of a run 74 seconds old and look for the stale wording.
    let status = run_status("1:14");

    assert!(!status.contains("Saved"), "{status}");
    assert!(!status.contains("No live connection"), "{status}");
    assert_eq!(
        status,
        "Flightdeck diagnostic run · 1:14 elapsed · 10-second reports from the collector this \
         app started."
    );
}
