use super::{METRICS, Metric, Ranking, Ranks};
use crate::apps::{Subtotal, Totals};
use crate::programs::{AppKey, Capacity, Program, Programs};

const NS: u64 = 1_000_000_000;
const CAPACITY: Capacity = Capacity {
    entries: 1024,
    program_bytes: 64 * 1024,
};

#[test]
fn ranks_the_largest_apps_wherever_they_sit_in_the_tick() {
    // Goal: every metric ranks the ten largest Apps of the whole tick, the most first.
    // Method: rank six hundred Apps whose totals rise with their position.
    let mut programs = Programs::new(CAPACITY);
    let totals = (0..600)
        .map(|amount| app(&mut programs, amount))
        .collect::<Vec<Totals>>();

    let ranks = rank(&programs, &totals, Some(NS));

    let largest = (590..600).rev().collect::<Vec<u64>>();
    assert_eq!(ranks.app_count, 600);
    assert_eq!(
        ranks.metrics.iter().map(amounts).collect::<Vec<_>>(),
        vec![largest; 5]
    );
}

#[test]
fn ranks_each_metric_by_its_own_measure() {
    // Goal: CPU, resident memory, power, disk and GPU each rank the App that used most of it.
    // Method: rank Apps that each lead one measure, beside one that leads the measures no
    // ranking reads.
    let mut programs = Programs::new(CAPACITY);
    let mut cpu = named(&mut programs, b"cpu", 1);
    cpu.cpu_ns.known = 50;
    let mut resident = named(&mut programs, b"resident", 2);
    resident.resident_bytes.known = 50;
    let mut power = named(&mut programs, b"power", 3);
    power.energy_nj.known = 50;
    let mut disk = named(&mut programs, b"disk", 4);
    disk.disk_io_bytes.known = 50;
    let mut gpu = named(&mut programs, b"gpu", 6);
    gpu.gpu_ns.known = 50;
    let mut other = named(&mut programs, b"other", 5);
    other.footprint_bytes.known = 90;
    other.disk_read_bytes.known = 90;
    other.disk_written_bytes.known = 90;
    other.idle_wakeups.known = 90;
    other.pageins.known = 90;
    other.threads.known = 90;

    let ranks = rank(
        &programs,
        &[cpu, resident, power, disk, gpu, other],
        Some(NS),
    );

    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| (ranking.metric, keys(&programs, ranking)[0].clone()))
            .collect::<Vec<_>>(),
        vec![
            (Metric::Cpu, b"cpu".to_vec()),
            (Metric::Resident, b"resident".to_vec()),
            (Metric::Power, b"power".to_vec()),
            (Metric::Disk, b"disk".to_vec()),
            (Metric::Gpu, b"gpu".to_vec()),
        ]
    );
}

#[test]
fn holds_the_rankings_in_the_order_of_the_metrics() {
    // Goal: a reader finds each metric's ranking at that metric's place among the metrics.
    // Method: rank no Apps and read the metric of each ranking.
    let programs = Programs::new(CAPACITY);

    let ranks = rank(&programs, &[], Some(NS));

    assert_eq!(
        ranks.metrics.each_ref().map(|ranking| ranking.metric),
        METRICS
    );
    assert_eq!(ranks.app_count, 0);
}

#[test]
fn gives_each_rank_its_rate_over_the_elapsed_interval() {
    // Goal: a rank carries the rate the App page shows, and resident memory carries none.
    // Method: rank one App with known CPU, energy, disk and GPU over one elapsed second.
    let mut programs = Programs::new(CAPACITY);
    let mut app = app(&mut programs, 1);
    app.cpu_ns.known = 500_000_000;
    app.energy_nj.known = 3_000_000_000;
    app.disk_io_bytes.known = 1200;
    app.gpu_ns.known = 250_000_000;

    let ranks = rank(&programs, &[app], Some(NS));

    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| ranking.entries()[0].rate)
            .collect::<Vec<_>>(),
        vec![Some(50.0), None, Some(3.0), Some(1200.0), Some(25.0)]
    );
}

#[test]
fn ranks_equal_totals_by_their_app_keys_whatever_order_the_apps_came_in() {
    // Goal: equal totals rank the same way every time, whatever order the Apps were met in.
    // Method: rank two equal Apps whose index numbers sort the other way from their keys.
    let mut programs = Programs::new(CAPACITY);
    let first = named(&mut programs, b"b", 7);
    let second = named(&mut programs, b"a", 7);

    let ranks = rank(&programs, &[first, second], Some(NS));

    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| keys(&programs, ranking))
            .collect::<Vec<_>>(),
        vec![vec![b"a".to_vec(), b"b".to_vec()]; 5]
    );
}

#[test]
fn ranks_a_bundle_before_an_executable_with_an_equal_total() {
    // Goal: the kind of an App's key orders equal totals before its bytes do.
    // Method: rank an executable whose key bytes sort before an equal bundle's.
    let mut programs = Programs::new(CAPACITY);
    let executable = named(&mut programs, b"a", 7);
    let entry = programs
        .intern(Program {
            executable_name: b"b",
            app_path: Some(b"b"),
        })
        .unwrap();
    let bundle = totals(programs.app(entry), 7);

    let ranks = rank(&programs, &[executable, bundle], Some(NS));

    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| ranking
                .entries()
                .iter()
                .map(|rank| programs.key(rank.app))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![vec![AppKey::Bundle(b"b"), AppKey::Executable(b"a")]; 5]
    );
}

#[test]
fn ranks_an_app_tied_with_the_tenth_by_its_key_after_ten_have_filled_the_ranks() {
    // Goal: an App whose total equals the tenth rank's takes that rank when its key sorts
    // first, wherever it sits in the tick.
    // Method: rank ten equal Apps, then an eleventh with their total whose key sorts first.
    let mut programs = Programs::new(CAPACITY);
    let mut totals = (2..=11)
        .map(|byte| named(&mut programs, &[byte], 7))
        .collect::<Vec<Totals>>();
    totals.push(named(&mut programs, &[1], 7));

    let ranks = rank(&programs, &totals, Some(NS));

    let first_ten = (1..=10).map(|byte| vec![byte]).collect::<Vec<Vec<u8>>>();
    assert_eq!(
        ranks
            .metrics
            .iter()
            .map(|ranking| keys(&programs, ranking))
            .collect::<Vec<_>>(),
        vec![first_ten; 5]
    );
}

#[test]
fn ranks_by_exact_totals_where_their_rates_are_equal() {
    // Goal: a difference of one above float precision still decides the order.
    // Method: rank two CPU totals that differ by one past the 53-bit mantissa.
    let mut programs = Programs::new(CAPACITY);
    let mut first = app(&mut programs, 1);
    first.cpu_ns.known = 9_007_199_254_740_992;
    let mut second = app(&mut programs, 2);
    second.cpu_ns.known = 9_007_199_254_740_993;

    let ranks = rank(&programs, &[first, second], Some(NS));

    assert_eq!(
        amounts(&ranks.metrics[0]),
        vec![9_007_199_254_740_993, 9_007_199_254_740_992]
    );
}

#[test]
fn keeps_measured_zero_partial_unmeasured_and_overflowed_totals_apart() {
    // Goal: each metric ranks only what it measured, and counts what it could not rank.
    // Method: rank a measured zero, an overflow, an unmeasured total and a partial one in CPU.
    let mut programs = Programs::new(CAPACITY);
    let zero = app(&mut programs, 0);
    let mut overflowed = app(&mut programs, 1);
    overflowed.cpu_ns.overflowed = true;
    let mut unmeasured = app(&mut programs, 2);
    unmeasured.cpu_ns.measured_members = 0;
    let mut partial = app(&mut programs, 3);
    partial.cpu_ns.unavailable_members = 2;

    let ranks = rank(
        &programs,
        &[zero, overflowed, unmeasured, partial],
        Some(NS),
    );

    let cpu = &ranks.metrics[0];
    assert_eq!((cpu.unavailable, cpu.overflowed, cpu.partial), (1, 1, 1));
    assert_eq!(amounts(cpu), vec![3, 0]);
    assert_eq!(cpu.entries()[0].unavailable_members, 2);
    assert_eq!(cpu.entries()[1].rate, Some(0.0));
}

#[test]
fn ranks_an_app_in_one_metric_whatever_another_metric_could_not_measure() {
    // Goal: a CPU total that could not be ranked leaves the App's other totals ranked.
    // Method: rank an overflowed and an unmeasured CPU total, and read resident memory.
    let mut programs = Programs::new(CAPACITY);
    let mut overflowed = app(&mut programs, 1);
    overflowed.cpu_ns.overflowed = true;
    let mut unmeasured = app(&mut programs, 2);
    unmeasured.cpu_ns.measured_members = 0;

    let ranks = rank(&programs, &[overflowed, unmeasured], Some(NS));

    assert_eq!(amounts(&ranks.metrics[1]), vec![2, 1]);
}

#[test]
fn ranks_no_rate_without_an_elapsed_interval_except_resident_memory() {
    // Goal: a first observation's counters never rank as measured zero rates.
    // Method: rank one App of a tick with no elapsed interval, and one of a tick whose
    // interval is zero.
    let mut programs = Programs::new(CAPACITY);
    let totals = [app(&mut programs, 0)];

    for elapsed_ns in [None, Some(0)] {
        let ranks = rank(&programs, &totals, elapsed_ns);

        assert_eq!(
            ranks
                .metrics
                .iter()
                .map(|ranking| ranking.unavailable)
                .collect::<Vec<_>>(),
            vec![1, 0, 1, 1, 1]
        );
        assert_eq!(amounts(&ranks.metrics[1]), vec![0]);
    }
}

#[test]
fn ranks_each_tick_afresh_in_the_buffers_of_the_last() {
    // Goal: ranking a tick keeps nothing of the tick ranked before it.
    // Method: rank twelve Apps with one unmeasured, then one App, in the same ranks.
    let mut programs = Programs::new(CAPACITY);
    let mut first = (0..12)
        .map(|amount| app(&mut programs, amount))
        .collect::<Vec<Totals>>();
    first[0].cpu_ns.measured_members = 0;
    let second = [app(&mut programs, 99)];
    let mut ranks = Ranks::default();
    ranks.rank(&programs, &first, Some(NS));

    ranks.rank(&programs, &second, Some(NS));

    assert_eq!(ranks, rank(&programs, &second, Some(NS)));
    assert_eq!(ranks.metrics[0].entries().len(), 1);
    assert_eq!(ranks.metrics[0].unavailable, 0);
}

fn rank(programs: &Programs, totals: &[Totals], elapsed_ns: Option<u64>) -> Ranks {
    let mut ranks = Ranks::default();
    ranks.rank(programs, totals, elapsed_ns);
    ranks
}

fn amounts(ranking: &Ranking) -> Vec<u64> {
    ranking.entries().iter().map(|rank| rank.amount).collect()
}

/// The key bytes of each ranked App, which the tests name by an executable's name.
fn keys(programs: &Programs, ranking: &Ranking) -> Vec<Vec<u8>> {
    ranking
        .entries()
        .iter()
        .map(|rank| match programs.key(rank.app) {
            AppKey::Bundle(bytes) | AppKey::Executable(bytes) => bytes.to_vec(),
        })
        .collect()
}

/// An App outside any bundle whose every total is `amount`, named by its amount's bytes so
/// its key sorts with its amount.
fn app(programs: &mut Programs, amount: u64) -> Totals {
    named(programs, &amount.to_be_bytes(), amount)
}

/// An App of the executable `name` outside any bundle, whose every total is `amount`.
fn named(programs: &mut Programs, name: &[u8], amount: u64) -> Totals {
    let entry = programs
        .intern(Program {
            executable_name: name,
            app_path: None,
        })
        .unwrap();
    totals(programs.app(entry), amount)
}

fn totals(app: crate::programs::App, amount: u64) -> Totals {
    let measure = Subtotal {
        known: amount,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    Totals {
        app,
        members: 1,
        live_members: 1,
        cpu_ns: measure.clone(),
        energy_nj: measure.clone(),
        gpu_ns: measure.clone(),
        disk_read_bytes: measure.clone(),
        disk_written_bytes: measure.clone(),
        disk_io_bytes: measure.clone(),
        idle_wakeups: measure.clone(),
        pageins: measure.clone(),
        resident_bytes: measure.clone(),
        footprint_bytes: measure.clone(),
        threads: measure,
    }
}
