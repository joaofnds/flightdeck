use super::{MinuteReport, Seal, SelfReports, TICK_REPORTS_MAX};

const HEADER: &str = "ticks 11-20: collector CPU 1.31% of a core, 26% of the combined budget, \
    38.42 mW, 100% of instructions on Super cores; ticks late 6.2 ms on average, 10.0 ms at \
    most, 0 slips";
const SOURCES: &str = "  ms of CPU a second: processes 5.76, coalitions 1.85, events 0.03, \
    scans 0.08, start reads 0.01, exec reads 0.00, watches 0.00, exit reads 0.01, ioreport \
    4.75, smc 0.09, machine 0.03, interfaces 0.11, ring 0.13, battery 0.00, disks 0.26, \
    unattributed 0.01";
const COVERAGE: &str = "  last tick read 755 processes (360 refused, 2 gone), 716 coalitions, \
    18 CPUs, 39 interfaces; over these ticks 8 forks, 18 rescans (8 leading, 0 trailing, 10 \
    listing), 8 starts (0 watches refused), 4 execs, exits seen 10 (4 read, 4 refused, 2 \
    gone), found already exited 0 (0 read, 0 refused, 0 gone); 1 starts, 2 execs and 3 \
    exits dropped from a full event list";

fn read(reports: &mut SelfReports, lines: &[&str], second: u64) {
    for line in lines {
        reports.standard(line, second);
    }
}

#[test]
fn a_ten_second_report_becomes_one_tick_report() {
    // Goal: the three lines of a ten-second report give its cost, lateness and coverage.
    // Method: read a report the collector wrote and inspect the one tick report it makes.
    let mut reports = SelfReports::default();

    read(&mut reports, &[HEADER, SOURCES, COVERAGE], 1_000);

    assert_eq!(reports.ticks.len(), 1);
    let tick = &reports.ticks[0];
    assert_eq!(
        (tick.tick_first, tick.tick_last, tick.second),
        (11, 20, 1_000)
    );
    assert_eq!(
        (tick.collector_cpu_percent, tick.collector_milliwatts),
        (1.31, 38.42)
    );
    assert_eq!(
        (tick.late_average_ms, tick.late_maximum_ms, tick.slips),
        (6.2, 10.0, 0)
    );
    assert_eq!(tick.sources.len(), 16);
    assert_eq!(tick.sources[0], ("processes".to_owned(), 5.76));
    assert_eq!(tick.sources[4], ("start reads".to_owned(), 0.01));
    assert_eq!(tick.sources[15], ("unattributed".to_owned(), 0.01));
    assert_eq!(
        (
            tick.processes_observed,
            tick.processes_refused,
            tick.processes_gone
        ),
        (755, 360, 2)
    );
    assert_eq!(tick.event_drops, 6);
}

#[test]
fn the_minute_summary_of_ticks_adds_no_ten_second_report() {
    // Goal: the minute's own tick summary is not mistaken for a ten-second report.
    // Method: read a ten-second report, then the minute's summary in the same three lines.
    let mut reports = SelfReports::default();
    let minute = "minute, ticks 1-60: collector CPU 3.16% of a core, 63% of the combined \
        budget, 59.05 mW, 61% of instructions on Super cores; ticks late 5.8 ms on average, \
        10.2 ms at most, 0 slips";

    read(
        &mut reports,
        &[HEADER, SOURCES, COVERAGE, minute, SOURCES, COVERAGE],
        1_000,
    );

    assert_eq!(reports.ticks.len(), 1);
    assert_eq!(reports.ticks[0].tick_last, 20);
}

#[test]
fn a_report_cut_short_by_the_next_is_dropped() {
    // Goal: a report missing its coverage line never stands as a whole report.
    // Method: read a header and its sources, then a whole report, and count the reports.
    let mut reports = SelfReports::default();
    let next = HEADER.replace("11-20", "21-30");

    read(
        &mut reports,
        &[HEADER, SOURCES, &next, SOURCES, COVERAGE],
        1_000,
    );

    assert_eq!(reports.ticks.len(), 1);
    assert_eq!(reports.ticks[0].tick_first, 21);
}

#[test]
fn a_malformed_header_starts_no_report() {
    // Goal: a header whose numbers do not parse yields nothing rather than a made-up reading.
    // Method: read a report whose collector CPU is not a number.
    let mut reports = SelfReports::default();
    let header = HEADER.replace("1.31%", "n/a%");

    read(&mut reports, &[&header, SOURCES, COVERAGE], 1_000);

    assert!(reports.ticks.is_empty());
}

#[test]
fn a_minute_joins_its_cost_and_the_persister_footprint() {
    // Goal: a minute report carries both CPU costs and the persister's footprint.
    // Method: read the collector's minute line and the cumulative line after it.
    let mut reports = SelfReports::default();

    read(
        &mut reports,
        &[
            "recording minute elapsed_ns=60015526708 collector_cpu_ns=1897702834 \
             persister_cpu_ns=39109625 combined_cpu_ns=1936812459 combined_percent=3.227186",
            "recording cumulative collector_cpu_ns=2127517458 persister_cpu_ns=39109625 \
             current_footprint_bytes=4112936 lifetime_footprint_bytes_max=4850288",
        ],
        1_060,
    );

    assert_eq!(
        reports.minutes.iter().collect::<Vec<_>>(),
        vec![&MinuteReport {
            second: 1_060,
            elapsed_ns: 60_015_526_708,
            collector_cpu_ns: 1_897_702_834,
            persister_cpu_ns: 39_109_625,
            combined_percent: 3.227_186,
            footprint_bytes: Some(4_112_936),
            footprint_bytes_max: Some(4_850_288),
        }]
    );
}

#[test]
fn the_cost_of_the_final_partial_window_is_not_a_minute() {
    // Goal: the tail written as a run ends never counts as a minute of the recording.
    // Method: read a tail line and the cumulative line after it.
    let mut reports = SelfReports::default();

    read(
        &mut reports,
        &[
            "recording tail elapsed_ns=14642130125 collector_cpu_ns=339950708 \
             persister_cpu_ns=248295958 combined_cpu_ns=588246666 combined_percent=4.017494",
            "recording cumulative collector_cpu_ns=2467468166 persister_cpu_ns=287405583 \
             current_footprint_bytes=51102296 lifetime_footprint_bytes_max=80593592",
        ],
        1_075,
    );

    assert!(reports.minutes.is_empty());
}

#[test]
fn a_persister_seal_line_becomes_a_seal() {
    // Goal: every sealed window keeps its publications, losses, seal time and staged bytes.
    // Method: read the persister's seal line and an unrelated diagnostic line.
    let mut reports = SelfReports::default();

    reports.diagnostic(
        "persister sealed window=1 publications=61..76 loss=2 events_lost=3 \
         seal_ns=126668250 staged_bytes=74300480",
    );
    reports.diagnostic("persister finished footprint_bytes=53199448 footprint_bytes_max=1");

    assert_eq!(
        reports.seals.iter().collect::<Vec<_>>(),
        vec![&Seal {
            window: 1,
            publication_first: 61,
            publication_end: 76,
            publications_lost: 2,
            events_lost: 3,
            sealing_ns: 126_668_250,
            staged_bytes: 74_300_480,
        }]
    );
}

#[test]
fn the_store_keeps_only_the_last_fifteen_minutes_of_reports() {
    // Goal: the reports stay bounded, keeping the newest.
    // Method: read one report more than the bound and look at both ends.
    let mut reports = SelfReports::default();

    for index in 0..=TICK_REPORTS_MAX {
        let second = u64::try_from(index).unwrap_or(u64::MAX);
        read(&mut reports, &[HEADER, SOURCES, COVERAGE], second);
    }

    assert_eq!(reports.ticks.len(), TICK_REPORTS_MAX);
    assert_eq!(reports.ticks.front().map(|tick| tick.second), Some(1));
    assert_eq!(
        reports.ticks.back().map(|tick| tick.second),
        u64::try_from(TICK_REPORTS_MAX).ok()
    );
}

#[test]
fn beginning_a_run_forgets_the_last_run() {
    // Goal: a new collector run starts with no reports from the one before.
    // Method: read a report, begin a run, and read what the store holds.
    let mut reports = SelfReports::default();
    read(&mut reports, &[HEADER, SOURCES, COVERAGE], 1_000);

    reports.begin(2_000);

    assert!(reports.ticks.is_empty());
    assert_eq!(reports.started, Some(2_000));
}

#[test]
fn a_minute_shares_the_second_of_the_report_that_closes_it() {
    // Goal: inspecting the report that ends a minute finds that minute too.
    // Method: read a report in one second and the minute's lines in the next.
    let mut reports = SelfReports::default();
    read(&mut reports, &[HEADER, SOURCES, COVERAGE], 1_000);

    read(
        &mut reports,
        &[
            "recording minute elapsed_ns=1 collector_cpu_ns=1 persister_cpu_ns=1 \
             combined_cpu_ns=2 combined_percent=1.5",
            "recording cumulative current_footprint_bytes=1 lifetime_footprint_bytes_max=2",
        ],
        1_001,
    );

    assert_eq!(
        reports.minutes.front().map(|minute| minute.second),
        Some(1_000)
    );
}

#[test]
fn seal_totals_count_every_window_and_its_losses() {
    // Goal: the run's totals keep counting after the kept seals reach their bound.
    // Method: read more full seals than the bound and one partial seal with losses.
    let mut reports = SelfReports::default();
    let full = "persister sealed window=0 publications=1..61 loss=0 events_lost=0 seal_ns=1 \
        staged_bytes=1";

    for _ in 0..super::SEALS_MAX {
        reports.diagnostic(full);
    }
    reports.diagnostic(
        "persister sealed window=9 publications=61..62 loss=2 events_lost=3 seal_ns=1 \
         staged_bytes=1",
    );

    assert_eq!(reports.seals.len(), super::SEALS_MAX);
    assert_eq!(
        reports.seal_totals,
        super::SealTotals {
            windows: 17,
            partial: 1,
            publications_lost: 2,
            events_lost: 3,
        }
    );
}
