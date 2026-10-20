use super::*;
use crate::live::window::WindowValue;

const NS: u64 = 1_000_000_000;

#[test]
fn ranks_the_largest_apps_wherever_they_sit_in_the_sample() {
    // Goal: every metric ranks the ten largest Apps of the whole sample, the most first.
    // Method: rank six hundred Apps whose totals rise with their position.
    let sample = sample((0..600).map(app).collect());

    let ranks = sample.ranks().unwrap();

    let largest = (590..600).rev().collect::<Vec<u64>>();
    assert_eq!(ranks.app_count, 600);
    assert_eq!(
        ranks.metrics.iter().map(amounts).collect::<Vec<_>>(),
        vec![largest; 4]
    );
}

#[test]
fn ranks_each_metric_by_its_own_measure() {
    // Goal: CPU, resident memory, power and disk each rank the App that used most of it.
    // Method: rank Apps that each lead one measure, beside one that leads the measures no
    // ranking reads.
    let mut cpu = app(1);
    cpu.display_name = "cpu".into();
    cpu.cpu_ns.known = 50;
    let mut resident = app(2);
    resident.display_name = "resident".into();
    resident.resident_bytes.known = 50;
    let mut power = app(3);
    power.display_name = "power".into();
    power.energy_nj.known = 50;
    let mut disk = app(4);
    disk.display_name = "disk".into();
    disk.disk_io_bytes.known = 50;
    let mut other = app(5);
    other.display_name = "other".into();
    other.footprint_bytes.known = 90;
    other.disk_read_bytes.known = 90;
    other.disk_written_bytes.known = 90;

    let ranks = sample(vec![cpu, resident, power, disk, other])
        .ranks()
        .unwrap();

    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| (ranking.metric, names(ranking)[0]))
            .collect::<Vec<_>>(),
        vec![
            (Metric::Cpu, "cpu"),
            (Metric::Resident, "resident"),
            (Metric::Power, "power"),
            (Metric::Disk, "disk")
        ]
    );
}

#[test]
fn gives_each_rank_its_rate_over_the_elapsed_interval() {
    // Goal: a rank carries the rate the App page shows, and resident memory carries none.
    // Method: rank one App with known CPU, energy and disk over one elapsed second.
    let mut app = app(1);
    app.cpu_ns.known = 500_000_000;
    app.energy_nj.known = 3_000_000_000;
    app.disk_io_bytes.known = 1200;

    let ranks = sample(vec![app]).ranks().unwrap();

    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| ranking.entries[0].rate)
            .collect::<Vec<_>>(),
        vec![Some(50.0), None, Some(3.0), Some(1200.0)]
    );
}

#[test]
fn ranks_equal_totals_by_their_app_keys_rather_than_their_names() {
    // Goal: equal totals rank the same way every time, whatever the Apps are called.
    // Method: rank two equal Apps whose names sort the other way from their keys.
    let mut first = app(7);
    first.key.bytes = vec![2];
    first.display_name = "A".into();
    let mut second = app(7);
    second.key.bytes = vec![1];
    second.display_name = "Z".into();

    let ranks = sample(vec![first, second]).ranks().unwrap();

    assert_eq!(
        ranks.metrics.iter().map(names).collect::<Vec<_>>(),
        vec![vec!["Z", "A"]; 4]
    );
}

#[test]
fn ranks_a_bundle_before_an_executable_with_an_equal_total() {
    // Goal: the kind of an App's key orders equal totals before its bytes do.
    // Method: rank an executable whose key bytes sort before an equal bundle's.
    let mut executable = app(7);
    executable.key.bytes = vec![1];
    executable.display_name = "executable".into();
    let mut bundle = app(7);
    bundle.key.kind = KeyKind::Bundle;
    bundle.key.bytes = vec![2];
    bundle.display_name = "bundle".into();

    let ranks = sample(vec![executable, bundle]).ranks().unwrap();

    assert_eq!(
        ranks.metrics.iter().map(names).collect::<Vec<_>>(),
        vec![vec!["bundle", "executable"]; 4]
    );
}

#[test]
fn ranks_an_app_tied_with_the_tenth_by_its_key_after_ten_have_filled_the_ranks() {
    // Goal: an App whose total equals the tenth rank's takes that rank when its key sorts
    // first, wherever it sits in the sample.
    // Method: rank ten equal Apps, then an eleventh with their total whose key sorts first.
    let mut apps = (2..=11).map(tied_app).collect::<Vec<App>>();
    apps.push(tied_app(1));

    let ranks = sample(apps).ranks().unwrap();

    let first_ten = (1..=10).map(|byte| vec![byte]).collect::<Vec<Vec<u8>>>();
    assert_eq!(
        ranks.metrics.iter().map(keys).collect::<Vec<_>>(),
        vec![first_ten; 4]
    );
}

#[test]
fn ranks_by_exact_totals_where_their_rates_are_equal() {
    // Goal: a difference of one above float precision still decides the order.
    // Method: rank two CPU totals that differ by one past the 53-bit mantissa.
    let mut first = app(1);
    first.cpu_ns.known = 9_007_199_254_740_992;
    let mut second = app(2);
    second.cpu_ns.known = 9_007_199_254_740_993;

    let ranks = sample(vec![first, second]).ranks().unwrap();

    assert_eq!(
        amounts(&ranks.metrics[0]),
        vec![9_007_199_254_740_993, 9_007_199_254_740_992]
    );
}

#[test]
fn keeps_measured_zero_partial_unmeasured_and_overflowed_totals_apart() {
    // Goal: each metric ranks only what it measured, and counts what it could not rank.
    // Method: rank a measured zero, an overflow, an unmeasured total and a partial one in CPU.
    let zero = app(0);
    let mut overflowed = app(1);
    overflowed.cpu_ns.overflowed = true;
    let mut unmeasured = app(2);
    unmeasured.cpu_ns.measured_members = 0;
    let mut partial = app(3);
    partial.cpu_ns.unavailable_members = 2;

    let ranks = sample(vec![zero, overflowed, unmeasured, partial])
        .ranks()
        .unwrap();

    let cpu = &ranks.metrics[0];
    assert_eq!((cpu.unavailable, cpu.overflowed, cpu.partial), (1, 1, 1));
    assert_eq!(amounts(cpu), vec![3, 0]);
    assert_eq!(cpu.entries[0].unavailable_members, 2);
    assert_eq!(cpu.entries[1].rate, Some(0.0));
}

#[test]
fn ranks_an_app_in_one_metric_whatever_another_metric_could_not_measure() {
    // Goal: a CPU total that could not be ranked leaves the App's other totals ranked.
    // Method: rank an overflowed and an unmeasured CPU total, and read resident memory.
    let mut overflowed = app(1);
    overflowed.cpu_ns.overflowed = true;
    let mut unmeasured = app(2);
    unmeasured.cpu_ns.measured_members = 0;

    let ranks = sample(vec![overflowed, unmeasured]).ranks().unwrap();

    assert_eq!(amounts(&ranks.metrics[1]), vec![2, 1]);
}

#[test]
fn ranks_no_rate_without_an_elapsed_interval_except_resident_memory() {
    // Goal: a first observation's counters never rank as measured zero rates.
    // Method: rank one App of a sample with no elapsed interval.
    let mut sample = sample(vec![app(0)]);
    sample.elapsed_ns = None;

    let ranks = sample.ranks().unwrap();

    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| ranking.unavailable)
            .collect::<Vec<_>>(),
        vec![1, 0, 1, 1]
    );
    assert_eq!(amounts(&ranks.metrics[1]), vec![0]);
}

#[test]
fn keeps_the_samples_membership_coverage() {
    // Goal: ranks from a sample with unknown program identities say they are partial.
    // Method: rank a sample with incomplete membership and seven ungrouped processes.
    let mut sample = sample(vec![app(1)]);
    sample.membership_complete = false;
    sample.ungrouped_members = 7;

    let ranks = sample.ranks().unwrap();

    assert!(!ranks.membership_complete);
    assert_eq!(ranks.ungrouped_members, 7);
}

#[test]
fn counts_the_name_of_each_app_it_ranks_in_its_bytes() {
    // Goal: the bound on a window's ranks sees the names they keep, once in each ranking.
    // Method: rank one App under its eight-byte name and under a name a kibibyte long.
    let short = sample(vec![app(1)]).ranks().unwrap();
    let mut named = app(1);
    named.display_name = "A".repeat(1024);

    let long = sample(vec![named]).ranks().unwrap();

    assert_eq!(long.bytes() - short.bytes(), 4 * (1024 - 8));
}

fn amounts(ranking: &Ranking) -> Vec<u64> {
    ranking.entries.iter().map(|rank| rank.amount).collect()
}

fn keys(ranking: &Ranking) -> Vec<Vec<u8>> {
    ranking
        .entries
        .iter()
        .map(|rank| rank.app.bytes.clone())
        .collect()
}

fn names(ranking: &Ranking) -> Vec<&str> {
    ranking
        .entries
        .iter()
        .map(|rank| rank.app.display_name.as_str())
        .collect()
}

fn sample(apps: Vec<App>) -> Sample {
    Sample {
        selected: Observation {
            recording: [1, 2],
            stamp: Stamp {
                boot: [3, 4],
                run: [5, 6],
                publication: 2,
                continuous_ns: Some(2 * NS),
                awake_ns: Some(2 * NS),
            },
            run_tick: 1,
            wall_ns: Some(1001 * NS),
        },
        previous: None,
        elapsed_ns: Some(NS),
        membership_complete: true,
        ungrouped_members: 0,
        usage_refused: 0,
        usage_gone: 0,
        apps,
        processes: Vec::new(),
    }
}

/// An App with the total every tied App shares, told apart by its one key byte.
fn tied_app(byte: u8) -> App {
    let mut app = app(7);
    app.key.bytes = vec![byte];

    app
}

fn app(amount: u64) -> App {
    let measure = Measure {
        known: amount,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    App {
        key: Key {
            kind: KeyKind::Executable,
            bytes: amount.to_be_bytes().to_vec(),
            display: format!("App{amount:05}"),
        },
        display_name: format!("App{amount:05}"),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measure,
        energy_nj: measure,
        disk_read_bytes: measure,
        disk_written_bytes: measure,
        disk_io_bytes: measure,
        resident_bytes: measure,
        footprint_bytes: measure,
    }
}
