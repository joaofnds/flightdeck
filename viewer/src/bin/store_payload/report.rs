//! What the `store_payload` tool reports of the View it held last, apart from the tool's run so
//! a test reads the report of a View it builds.
use std::io;
use std::time::{Duration, Instant};

use persistence::resources::Footprint;
use serde_json::{Value, json};
use viewer::apps::Amount;
use viewer::store::{
    AppDetails, AppRow, AppsSecond, ByteBudget, KeptPlain, MachineSecond, Missing, Payload,
    ProcessAt, ProcessRow, Processes, SampledSecond, View,
};

#[derive(Default)]
pub(super) struct PayloadCounts {
    complete: usize,
    unread: usize,
    budget: usize,
    refused: usize,
    too_many: usize,
    processes: usize,
    /// The process rows the View's seconds hold, each counted once: a plain second's own rows
    /// and the first row of each process of a packed block, which is all a packed block holds
    /// as rows.
    processes_rows_held: usize,
    processes_max: usize,
    app_details: usize,
    app_details_distinct: usize,
}

/// What the tool reports of `view`, the View it held last, held on `budget`, with the process
/// footprint `footprint`.
pub(super) fn held_report(
    view: &View,
    budget: &ByteBudget,
    footprint: &Footprint,
) -> io::Result<Value> {
    let walk = RowsWalk::of(view);
    let walk_ns = u64::try_from(walk.elapsed.as_nanos()).map_err(io::Error::other)?;
    let details_walk = RowsWalk::of_app_details(view);
    let details_walk_ns =
        u64::try_from(details_walk.elapsed.as_nanos()).map_err(io::Error::other)?;
    let counts = PayloadCounts::of(view);
    let blocks = json!({
        "held": view.apps.block_count(),
        "packed": view.apps.packed_count(),
        "kept_plain_bound": view.apps.kept_plain_count(KeptPlain::Bound),
        "kept_plain_budget": view.apps.kept_plain_count(KeptPlain::Budget),
        "apps_plain": view.apps.apps_plain_count(),
    });
    let mut held = json!({
        "component_only": true,
        "phase": "held_view",
        "version": view.version,
        "recent": view.seconds.len(),
        "earlier": view.earlier.len(),
        "sampled": view.sampled.len(),
        "sampled_complete": view.sampled.iter().filter(|sample| sample.ranks().is_ok()).count(),
        "payload_complete": counts.complete,
        "payload_unread": counts.unread,
        "payload_budget": counts.budget,
        "payload_refused": counts.refused,
        "payload_too_many": counts.too_many,
        "process_rows": counts.processes,
        "process_rows_held": counts.processes_rows_held,
        "process_rows_max": counts.processes_max,
        "process_row_reserved_bytes": Payload::ROW_BYTES,
        "process_rows_walk_ns": walk_ns,
        "process_rows_walk_sum": walk.sum,
        "app_details_walk_ns": details_walk_ns,
        "app_details_walk_sum": details_walk.sum,
        "app_details_rows": counts.app_details,
        "app_details_rows_distinct": counts.app_details_distinct,
        "app_details_row_reserved_bytes": Payload::APP_DETAILS_BYTES,
        "app_row_capacity_bytes_by_reference": view.apps.seconds()
            .map(|apps| apps.app_row_capacity() * size_of::<AppRow>()).sum::<usize>(),
        "app_row_initialized_bytes_by_reference": view.apps.seconds()
            .map(|apps| apps.app_row_count() * size_of::<AppRow>()).sum::<usize>(),
        "reserved_bytes": budget.used(),
        "reserved_bytes_max": budget.maximum(),
        "reserved_bytes_peak": budget.used_peak(),
        "footprint_bytes": footprint.current_bytes,
        "footprint_peak_bytes": footprint.lifetime_bytes_max,
        "resident_bytes": footprint.resident_bytes,
        "failure": format!("{:?}", view.failure),
        "left_out": format!("{:?}", view.left_out),
        "type_bytes": {
            "process": size_of::<ProcessRow>(), "app": size_of::<AppRow>(),
            "app_details": size_of::<AppDetails>(), "apps_second": size_of::<AppsSecond>(),
            "machine_second": size_of::<MachineSecond>(),
            "sampled_second": size_of::<SampledSecond>(),
        },
    });
    // Set apart from the rest, which reaches the macro's recursion limit.
    held["blocks"] = blocks;

    Ok(held)
}

impl PayloadCounts {
    fn of(view: &View) -> Self {
        let mut counts = Self::default();
        for apps in view.apps.seconds() {
            match apps.processes() {
                Ok(processes) => {
                    counts.complete += 1;
                    counts.processes += processes.len();
                    counts.processes_max = counts.processes_max.max(processes.len());
                    counts.app_details += processes.own_details().len();
                }
                Err(Missing::Unread) => counts.unread += 1,
                Err(Missing::Budget) => counts.budget += 1,
                Err(Missing::Refused(_)) => counts.refused += 1,
                Err(Missing::TooMany) => counts.too_many += 1,
            }
        }
        counts.processes_rows_held = Self::of_distinct(view, counts.processes, |processes| {
            processes.iter().map(|process| process.row_address())
        });
        counts.app_details_distinct = Self::of_distinct(view, counts.app_details, |processes| {
            processes
                .own_details()
                .map(|details| std::ptr::from_ref(details).addr())
        });

        counts
    }

    /// How many distinct rows `rows` gives across the seconds of `view` that hold their
    /// processes, `count` of them in all, telling rows apart by where they lie in memory. A
    /// process of a packed block counts its first row in the block.
    fn of_distinct<'view, Addresses: Iterator<Item = usize>>(
        view: &'view View,
        count: usize,
        rows: impl Fn(Processes<'view>) -> Addresses,
    ) -> usize {
        let mut addresses = Vec::with_capacity(count);
        for processes in payloads(view) {
            addresses.extend(rows(processes));
        }
        assert_eq!(addresses.len(), count);

        Self::of_distinct_addresses(&mut addresses)
    }

    /// How many distinct addresses `addresses` holds, sorting it.
    pub(super) fn of_distinct_addresses(addresses: &mut [usize]) -> usize {
        addresses.sort_unstable();

        addresses.chunk_by(|left, right| left == right).count()
    }
}

/// The seconds of `view` that hold their process rows.
fn payloads(view: &View) -> impl Iterator<Item = Processes<'_>> {
    view.apps.seconds().filter_map(|apps| apps.processes().ok())
}

/// One reader's pass over every reading of every held row of one kind, taken before anything
/// else reads those rows, with a sum that shows what it read.
struct RowsWalk {
    elapsed: Duration,
    sum: u64,
}

impl RowsWalk {
    fn of(view: &View) -> Self {
        let started = Instant::now();
        let mut sum = 0_u64;
        for processes in payloads(view) {
            for process in processes.iter() {
                sum = sum.wrapping_add(Self::of_process(&process));
            }
        }

        Self {
            elapsed: started.elapsed(),
            sum,
        }
    }

    fn of_app_details(view: &View) -> Self {
        let started = Instant::now();
        let mut sum = 0_u64;
        for processes in payloads(view) {
            for details in processes.details() {
                sum = sum
                    .wrapping_add(u64::from(details.live_members))
                    .wrapping_add(details.disk_read_bytes.known)
                    .wrapping_add(details.disk_written_bytes.known)
                    .wrapping_add(details.footprint_bytes.known);
            }
        }

        Self {
            elapsed: started.elapsed(),
            sum,
        }
    }

    fn of_process(process: &ProcessAt<'_>) -> u64 {
        let amounts = [
            process.cpu_ns(),
            process.energy_nj(),
            process.gpu_ns(),
            process.disk_read_bytes(),
            process.disk_written_bytes(),
            process.disk_io_bytes(),
            process.resident_bytes(),
            process.footprint_bytes(),
            process.idle_wakeups(),
            process.pageins(),
            process.threads(),
        ];
        let mut sum = u64::from(process.pid().unsigned_abs());
        for amount in amounts {
            if let Amount::Known(known) = amount {
                sum = sum.wrapping_add(known);
            }
        }

        sum
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use persistence::resources::Footprint;
    use viewer::apps::Stamp;
    use viewer::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp};
    use viewer::store::{
        AppDetails, AppMetadata, AppRow, AppsSecond, AppsSpan, ByteBudget, Payload, Totals, View,
    };

    use super::held_report;

    const NS: u64 = 1_000_000_000;
    /// The first wall second of the first block, the first of thirty wall seconds.
    const FIRST_WALL_SECOND: u64 = 1_020;
    const SECONDS_PER_BLOCK: u64 = 3;
    const NAMES: [&str; 2] = ["/A.app", "/B.app"];

    #[test]
    fn the_report_counts_only_what_the_seconds_hold_of_a_block_of_each_form() {
        // Goal: the report reads a View with a packed block that packs its App rows and one
        // that holds them plain: it counts each block in its form, as App details the details
        // the seconds hold of their own and each of those once by address, and as App row
        // bytes only the rows the seconds hold, since a block that packs its App rows holds
        // both in its table.
        // Method: build two blocks of three seconds of two Apps, the first App's details the
        // same at every second and so shared, the second's other at each, pack the first
        // block's App rows and hold the second's plain, read the report, and compare its
        // figures with what the second block's seconds hold.
        let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
        let mut seconds = Vec::new();
        for block in 0..2 {
            let mut previous: Option<Arc<Payload>> = None;
            for offset in 0..SECONDS_PER_BLOCK {
                let tick = block * SECONDS_PER_BLOCK + offset;
                let wall_second = FIRST_WALL_SECOND + block * 30 + offset;
                let second = second_at(tick, wall_second, previous.as_deref(), &budget);
                previous = second.processes.as_ref().ok().map(Arc::clone);
                seconds.push(Arc::new(second));
            }
        }
        let apps = seconds
            .into_iter()
            .collect::<AppsSpan>()
            .packing_each_block(&budget, |block| block == 0);
        let view = View {
            version: 1,
            seconds: Vec::new(),
            apps,
            earlier: Vec::new(),
            stretches: Vec::new(),
            sampled: Vec::new(),
            apps_loaded: true,
            failure: None,
            left_out: None,
            ring_complete: false,
            full_detail_evicted_through_ns: None,
        };
        let footprint = Footprint {
            current_bytes: 0,
            lifetime_bytes_max: 0,
            resident_bytes: 0,
        };

        let report = held_report(&view, &budget, &footprint).unwrap();

        let plain_rows = usize::try_from(SECONDS_PER_BLOCK).unwrap() * NAMES.len();
        // The first App's details are one allocation over the block, the second's one a second.
        let distinct = 1 + usize::try_from(SECONDS_PER_BLOCK).unwrap();
        assert_eq!(report["blocks"]["held"], 2);
        assert_eq!(report["blocks"]["packed"], 2);
        assert_eq!(report["blocks"]["apps_plain"], 1);
        assert_eq!(report["app_details_rows"], plain_rows);
        assert_eq!(report["app_details_rows_distinct"], distinct);
        assert_eq!(
            report["app_row_initialized_bytes_by_reference"],
            plain_rows * size_of::<AppRow>()
        );
    }

    /// The second of run tick `tick` at `wall_second`, holding each App of `NAMES` with a
    /// payload of no process rows whose details share with `previous` where they read the same.
    fn second_at(
        tick: u64,
        wall_second: u64,
        previous: Option<&Payload>,
        budget: &Arc<ByteBudget>,
    ) -> AppsSecond {
        let observation = Observation {
            recording: [1, 2],
            stamp: Stamp {
                boot: [3, 4],
                run: [5, 6],
                publication: tick + 1,
                continuous_ns: Some(tick * NS),
                awake_ns: Some(tick * NS),
            },
            run_tick: tick,
            wall_ns: Some(wall_second * NS),
        };
        let apps: Vec<AppRow> = NAMES.iter().map(|name| row(name, tick)).collect();
        let details = (0..).zip(NAMES).map(|(app, _)| AppDetails {
            live_members: 1,
            disk_read_bytes: measure(app * tick),
            disk_written_bytes: measure(0),
            footprint_bytes: measure(0),
        });
        let payload = Payload::of_app_details(observation, details.collect(), previous, budget);

        AppsSecond {
            metadata: AppMetadata::default(),
            processes: Ok(Arc::new(payload.unwrap())),
            missing: None,
            observation: Some(observation),
            consecutive: tick != 0,
            elapsed_ns: Some(NS),
            membership_complete: true,
            ungrouped_members: 0,
            live_ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
            totals: Totals::of(&apps),
            apps: apps.try_into().unwrap(),
        }
    }

    /// The row of the bundle `name` at run tick `tick`, of one member that spent `tick` ns of
    /// CPU time.
    fn row(name: &str, tick: u64) -> AppRow {
        let app = App {
            key: Key {
                kind: KeyKind::Bundle,
                bytes: name.as_bytes().to_vec(),
                display: name.into(),
            },
            display_name: name.into(),
            members: 1,
            live_members: 1,
            membership_complete: true,
            cpu_ns: measure(tick),
            energy_nj: measure(0),
            gpu_ns: measure(0),
            disk_read_bytes: measure(0),
            disk_written_bytes: measure(0),
            disk_io_bytes: measure(0),
            resident_bytes: measure(0),
            footprint_bytes: measure(0),
        };
        let ranked = Arc::new(RankedApp::named(
            KeyKind::Bundle,
            app.key.bytes.clone(),
            app.display_name.clone(),
        ));

        AppRow::new(&app, ranked)
    }

    fn measure(known: u64) -> Measure {
        Measure {
            known,
            measured_members: 1,
            unavailable_members: 0,
            overflowed: false,
        }
    }
}
