//! Tests that the reads that add up each App of a packed block once, the histories' first pass
//! and the leaders, answer as reading the same seconds one at a time answers.

use std::sync::Arc;

use super::histories::{HistorySecond, response_attribution_reduce, response_resident_reduce};
use crate::app_attribution_history::AppAttributionMetric;
use crate::byte_budget::ByteBudget;
use crate::cancellation::Cancellation;
use crate::live_app_response::Selected;
use crate::store::payload::packed::tests::{APP_ROWS_PACKED_NEVER, apps_only, generated_block};
use crate::store::payload::packed::{APP_ROWS_PACKED_ALWAYS, AppRowsShare};
use crate::store::{
    AppsSecond, AppsSpan, AppsStretch, Block, Cohorts, Leaders, MachineSecond, Readings,
    WallSeconds,
};

const NS: u64 = 1_000_000_000;
const METRICS: [AppAttributionMetric; 4] = [
    AppAttributionMetric::Cpu,
    AppAttributionMetric::Power,
    AppAttributionMetric::Disk,
    AppAttributionMetric::Gpu,
];
/// The generated blocks each test reads, of 31 to 38 seconds.
const SEEDS: std::ops::Range<u64> = 0..8;
/// The first wall second of the first block, the last of a stretch, so the block's first four
/// seconds lie in one stretch and the rest in the next ones.
const FIRST_WALL_SECOND: u64 = 909;
/// The Apps each second of the blocks past the bound on Apps names.
const BOUND_APPS_PER_SECOND: usize = 1_000;

/// A history's response: its JSON, or the bound it failed on.
type Response = Result<String, &'static str>;

#[test]
fn each_app_of_a_packed_block_reaches_the_histories_as_its_seconds_one_at_a_time_add_it() {
    // Goal: a history's first pass over a block that packs its App rows, which adds each App
    // once, answers as the pass over the same seconds held plain, which adds them a second at a
    // time, over Apps that come and go, unmeasured measures and seconds without Apps.
    // Method: hold generated blocks in both forms and compare each history's response over all
    // their seconds, selected at the last.
    for seed in SEEDS {
        let seconds = generated_block(seed, &budget());
        let order: Vec<(usize, usize)> = (0..seconds.len()).map(|index| (0, index)).collect();

        let [packed, plain] = histories_of_both_forms(&[seconds], &order, *order.last().unwrap());

        assert!(packed.iter().all(Result::is_ok), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn a_range_that_ends_inside_a_packed_block_reaches_the_histories_alike() {
    // Goal: a history whose range ends inside a block that packs its App rows leaves out the
    // block's later seconds as the pass one second at a time does.
    // Method: select the middle second of generated blocks, held in both forms, and compare.
    for seed in SEEDS {
        let seconds = generated_block(seed, &budget());
        let order: Vec<(usize, usize)> = (0..seconds.len()).map(|index| (0, index)).collect();

        let [packed, plain] = histories_of_both_forms(&[seconds], &order, order[order.len() / 2]);

        assert!(packed.iter().all(Result::is_ok), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn a_range_that_starts_inside_a_packed_block_reaches_the_histories_alike() {
    // Goal: a history whose range starts inside a block that packs its App rows leaves out the
    // block's earlier seconds as the pass one second at a time does.
    // Method: read generated blocks, held in both forms, from a third of their seconds on, and
    // compare.
    for seed in SEEDS {
        let seconds = generated_block(seed, &budget());
        let order: Vec<(usize, usize)> = (seconds.len() / 3..seconds.len())
            .map(|index| (0, index))
            .collect();

        let [packed, plain] = histories_of_both_forms(&[seconds], &order, *order.last().unwrap());

        assert!(packed.iter().all(Result::is_ok), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn a_second_with_no_elapsed_time_inside_a_packed_block_reaches_the_histories_alike() {
    // Goal: a second whose interval is zero adds no amount to an App's mean in either pass,
    // and still names the App.
    // Method: give two seconds of generated blocks no elapsed time, hold them in both forms and
    // compare each history.
    for seed in SEEDS {
        let mut seconds = generated_block(seed, &budget());
        for index in [3, 9] {
            Arc::get_mut(&mut seconds[index]).unwrap().elapsed_ns = Some(0);
        }
        let order: Vec<(usize, usize)> = (0..seconds.len()).map(|index| (0, index)).collect();

        let [packed, plain] = histories_of_both_forms(&[seconds], &order, *order.last().unwrap());

        assert!(packed.iter().all(Result::is_ok), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn a_duplicate_inside_a_packed_block_fails_the_histories_alike() {
    // Goal: a second read twice inside a block that packs its App rows fails each history with
    // the error the pass one second at a time gives.
    // Method: read the seventh second of a generated block twice in a row, in both forms.
    let seconds = generated_block(3, &budget());
    let mut order: Vec<(usize, usize)> = (0..seconds.len()).map(|index| (0, index)).collect();
    order.insert(7, (0, 6));

    let [packed, plain] = histories_of_both_forms(&[seconds], &order, *order.last().unwrap());

    assert_eq!(
        packed,
        failed_on(
            "duplicate resident history observation",
            "duplicate attribution observation"
        )
    );
    assert_eq!(packed, plain);
}

#[test]
fn a_conflict_inside_a_packed_block_fails_the_histories_alike() {
    // Goal: two seconds of a block that name one observation and differ fail each history's
    // first pass on the conflicting observation, as the pass one second at a time does.
    // Method: give the eighth second of a generated block the seventh's observation with
    // another tick, hold it in both forms and compare.
    let mut seconds = generated_block(3, &budget());
    let mut observation = seconds[6].observation.unwrap();
    observation.run_tick += 100;
    Arc::get_mut(&mut seconds[7]).unwrap().observation = Some(observation);
    let order: Vec<(usize, usize)> = (0..seconds.len()).map(|index| (0, index)).collect();

    let [packed, plain] = histories_of_both_forms(&[seconds], &order, *order.last().unwrap());

    assert_eq!(
        packed,
        failed_on(
            "conflicting resident history observation",
            "conflicting attribution observation"
        )
    );
    assert_eq!(packed, plain);
}

#[test]
fn the_bound_on_apps_met_inside_a_packed_block_fails_the_histories_alike() {
    // Goal: the means' bound on Apps, met at a second inside a block that packs its App rows,
    // fails each history with the error the pass one second at a time gives.
    // Method: hold twelve blocks of two seconds that name 24,000 Apps between them and a
    // thirteenth whose second second names one more, in both forms, and compare.
    let blocks = past_the_bound_on_apps(&budget());
    let order: Vec<(usize, usize)> = (0..blocks.len())
        .flat_map(|block| (0..2).map(move |index| (block, index)))
        .collect();

    let [packed, plain] = histories_of_both_forms(&blocks, &order, *order.last().unwrap());

    assert_eq!(packed, past_the_bound());
    assert_eq!(packed, plain);
}

#[test]
fn the_bound_on_apps_met_before_a_duplicate_inside_a_packed_block_fails_the_histories_alike() {
    // Goal: where the means' bound on Apps is met at a second inside a block that packs its App
    // rows and a later second of the block is a duplicate, each history fails on the bound, as
    // the pass one second at a time does, rather than on the duplicate.
    // Method: hold the blocks past the bound in both forms, read the second that passes the
    // bound twice, and compare.
    let blocks = past_the_bound_on_apps(&budget());
    let mut order: Vec<(usize, usize)> = (0..blocks.len())
        .flat_map(|block| (0..2).map(move |index| (block, index)))
        .collect();
    order.push(*order.last().unwrap());

    let [packed, plain] = histories_of_both_forms(&blocks, &order, *order.last().unwrap());

    assert_eq!(packed, past_the_bound());
    assert_eq!(packed, plain);
}

#[test]
fn each_app_of_a_packed_block_reaches_the_leaders_as_its_seconds_one_at_a_time_add_it() {
    // Goal: the leaders over blocks that pack their App rows, which add each App once a block,
    // add up what the leaders over the same seconds held plain add up, and choose alike.
    // Method: hold the two halves of generated blocks as two blocks in both forms, so an App of
    // the second block adds to what the first left, and compare each App's sums and the
    // cohorts chosen over every second.
    for seed in SEEDS {
        let mut first = generated_block(seed, &budget());
        let second = first.split_off(first.len() / 2);

        let [packed, plain] = leaders_of_both_forms(&[first, second], &[], 0);

        assert!(!packed.0.cpu.is_empty(), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn a_second_with_no_elapsed_time_or_none_inside_a_packed_block_reaches_the_leaders_alike() {
    // Goal: a second whose interval is zero, and one that has no interval, add no amount to an
    // App's sums in either form, as the leaders over the seconds held plain add them.
    // Method: give one second of generated blocks no elapsed time and another none, hold the
    // blocks in both forms and compare each App's sums and the cohorts chosen.
    for seed in SEEDS {
        let mut seconds = generated_block(seed, &budget());
        Arc::get_mut(&mut seconds[3]).unwrap().elapsed_ns = Some(0);
        Arc::get_mut(&mut seconds[9]).unwrap().elapsed_ns = None;

        let [packed, plain] = leaders_of_both_forms(&[seconds], &[], 0);

        assert!(!packed.0.cpu.is_empty(), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn a_window_that_starts_inside_a_packed_block_reaches_the_leaders_alike() {
    // Goal: the leaders over a window whose first second lies inside a block that packs its App
    // rows add only the block's seconds from that one on, as the leaders over the seconds held
    // plain do.
    // Method: hold the two halves of generated blocks as two blocks in both forms, choose over a
    // window from the third wall second of the first block on, and compare each App's sums
    // and the cohorts chosen.
    for seed in SEEDS {
        let mut first = generated_block(seed, &budget());
        let second = first.split_off(first.len() / 2);

        let [packed, plain] = leaders_of_both_forms(&[first, second], &[], FIRST_WALL_SECOND + 3);

        assert!(!packed.0.cpu.is_empty(), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn a_stretch_that_cuts_a_packed_block_reaches_the_leaders_alike() {
    // Goal: the leaders add a stretch's seconds through the stretch and the rest of a block that
    // packs its App rows once an App, as they do for the block held plain.
    // Method: close the stretch of the first four seconds of generated blocks, hold the blocks
    // in both forms and compare the cohorts chosen.
    for seed in SEEDS {
        let seconds = generated_block(seed, &budget());
        let stretch = Arc::new(AppsStretch::of_seconds(
            FIRST_WALL_SECOND / 10,
            &seconds[..4],
        ));

        let [packed, plain] = leaders_of_both_forms(&[seconds], &[stretch], 0);

        assert!(!packed.0.cpu.is_empty(), "seed {seed}");
        assert_eq!(packed, plain, "seed {seed}");
    }
}

#[test]
fn the_bound_on_apps_met_inside_a_packed_block_refuses_the_leaders_alike() {
    // Goal: the leaders' bound on Apps, met inside a block that packs its App rows, refuses the
    // cohorts as it does for the block held plain.
    // Method: hold the blocks past the bound in both forms and compare the cohorts chosen.
    let blocks = past_the_bound_on_apps(&budget());

    let [packed, plain] = leaders_of_both_forms(&blocks, &[], 0);

    assert!(packed.0.refused);
    assert!(packed.1.is_none());
    assert_eq!(packed, plain);
}

#[test]
fn a_span_of_blocks_of_both_forms_reaches_the_histories_and_the_leaders_as_plain_blocks() {
    // Goal: a span whose blocks pack their App rows or hold them plain, in turn, answers each
    // history and the leaders as the same blocks all held plain answer, so a reader that
    // crosses from one form into the other reads each second's Apps of its own block.
    // Method: split generated blocks into a block of their first second that packs its App
    // rows, a block of the next half that holds them plain and a block of the rest that packs
    // them, so a cursor leaves the first block at the index the third block starts at, and
    // compare each history over every second, and each App's sums and the cohorts chosen.
    for seed in SEEDS {
        let mut first = generated_block(seed, &budget());
        let mut second = first.split_off(1);
        let third = second.split_off(second.len() / 2);
        let blocks = [first, second, third];
        let order: Vec<(usize, usize)> = blocks
            .iter()
            .enumerate()
            .flat_map(|(block, seconds)| (0..seconds.len()).map(move |index| (block, index)))
            .collect();
        let forms = || {
            [
                vec![
                    APP_ROWS_PACKED_ALWAYS,
                    APP_ROWS_PACKED_NEVER,
                    APP_ROWS_PACKED_ALWAYS,
                ],
                vec![APP_ROWS_PACKED_NEVER; 3],
            ]
        };

        let [mixed, plain] = histories_of(&blocks, &order, *order.last().unwrap(), forms());
        let [mixed_leaders, plain_leaders] = leaders_of(&blocks, &[], 0, forms());

        assert!(mixed.iter().all(Result::is_ok), "seed {seed}");
        assert_eq!(mixed, plain, "seed {seed}");
        assert!(!mixed_leaders.0.cpu.is_empty(), "seed {seed}");
        assert_eq!(mixed_leaders, plain_leaders, "seed {seed}");
    }
}

fn budget() -> Arc<ByteBudget> {
    Arc::new(ByteBudget::new(1024 * 1024 * 1024))
}

/// The wall time the store took the second at `index` of the block at `block` at, four seconds
/// a wall second, each block in thirty wall seconds of its own.
fn wall_ns(block: usize, index: usize) -> u64 {
    let block = u64::try_from(block).unwrap();
    let index = u64::try_from(index).unwrap();

    (FIRST_WALL_SECOND + 30 * block) * NS + index * NS / 4
}

/// The blocks of `blocks`, each the plain seconds of one block, each packed under its share of
/// `shares`.
fn blocks_under(blocks: &[Vec<Arc<AppsSecond>>], shares: &[AppRowsShare]) -> Vec<Arc<Block>> {
    assert_eq!(blocks.len(), shares.len());
    let budget = budget();

    blocks
        .iter()
        .zip(shares)
        .enumerate()
        .map(|(block, (seconds, share))| {
            let packed = Block::packed_of(seconds, |index| wall_ns(block, index), *share, &budget);
            let packs = packed.second(0).block_apps().is_some();
            assert_eq!(packs, *share == APP_ROWS_PACKED_ALWAYS, "block {block}");
            packed
        })
        .collect()
}

/// The shares of `count` blocks that every block packs its App rows under, and those that every
/// block holds them plain under.
fn both_forms(count: usize) -> [Vec<AppRowsShare>; 2] {
    [
        vec![APP_ROWS_PACKED_ALWAYS; count],
        vec![APP_ROWS_PACKED_NEVER; count],
    ]
}

/// Each history's response, as `histories_of` reads it, with every block's App rows packed and
/// then with them held plain.
fn histories_of_both_forms(
    blocks: &[Vec<Arc<AppsSecond>>],
    order: &[(usize, usize)],
    selected: (usize, usize),
) -> [Vec<Response>; 2] {
    histories_of(blocks, order, selected, both_forms(blocks.len()))
}

/// Each history's response, resident memory and then each metric's attribution, over the
/// seconds `order` names of `blocks`, each a block and an index in it, selected at the second
/// `selected` names, with the blocks packed under each of the two `forms`, a share for each
/// block.
fn histories_of(
    blocks: &[Vec<Arc<AppsSecond>>],
    order: &[(usize, usize)],
    selected: (usize, usize),
    forms: [Vec<AppRowsShare>; 2],
) -> [Vec<Response>; 2] {
    forms.map(|shares| {
        let held = blocks_under(blocks, &shares);
        let (block, index) = selected;
        let observation = held[block].second(index).observation().unwrap();
        let selected = Selected::ending_at(observation).unwrap();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
        let seconds = order.iter().map(|&(block, index)| {
            let apps = held[block].second(index);
            HistorySecond {
                observation: apps.observation().unwrap(),
                apps,
            }
        });

        let mut responses = vec![described(response_resident_reduce(
            seconds.clone(),
            &selected,
            &operation,
        ))];
        for metric in METRICS {
            responses.push(described(response_attribution_reduce(
                seconds.clone(),
                &selected,
                metric,
                &operation,
            )));
        }
        responses
    })
}

/// `response` as its JSON, or as the bound it failed on, the one error a history over held
/// seconds gives.
fn described(response: persistence::Result<serde_json::Value>) -> Response {
    match response {
        Ok(value) => Ok(value.to_string()),
        Err(persistence::Error::Bound(bound)) => Err(bound),
        Err(error) => panic!("a history over held seconds fails only on a bound: {error}"),
    }
}

/// The responses of the histories where the resident history fails on `resident` and each
/// metric's attribution on `attribution`.
fn failed_on(resident: &'static str, attribution: &'static str) -> Vec<Response> {
    std::iter::once(Err(resident))
        .chain(METRICS.map(|_| Err(attribution)))
        .collect()
}

/// The responses of the histories over the blocks past the bound on Apps.
fn past_the_bound() -> Vec<Response> {
    failed_on("resident history context Apps", "attribution context Apps")
}

/// The leaders, as `leaders_of` reads them, with every block's App rows packed and then with
/// them held plain.
fn leaders_of_both_forms(
    blocks: &[Vec<Arc<AppsSecond>>],
    stretches: &[Arc<AppsStretch>],
    first_wall_second: u64,
) -> [(Cohorts, Option<Vec<String>>); 2] {
    leaders_of(
        blocks,
        stretches,
        first_wall_second,
        both_forms(blocks.len()),
    )
}

/// The cohorts the leaders choose over the window of `blocks`, each the plain seconds of one
/// block, from `first_wall_second` to the last second, with the closed `stretches`, and each
/// App's sums they add up before they choose, with the blocks packed under each of the two
/// `forms`, a share for each block.
fn leaders_of(
    blocks: &[Vec<Arc<AppsSecond>>],
    stretches: &[Arc<AppsStretch>],
    first_wall_second: u64,
    forms: [Vec<AppRowsShare>; 2],
) -> [(Cohorts, Option<Vec<String>>); 2] {
    forms.map(|shares| {
        let span = AppsSpan::of_blocks(blocks_under(blocks, &shares));
        let machine: Vec<Arc<MachineSecond>> = blocks
            .iter()
            .enumerate()
            .flat_map(|(block, seconds)| {
                (0..seconds.len()).map(move |index| {
                    Arc::new(MachineSecond {
                        wall_ns: wall_ns(block, index),
                        ..MachineSecond::default()
                    })
                })
            })
            .collect();
        let readings = Readings {
            seconds: &machine,
            apps: &span,
            stretches,
        };
        let window = WallSeconds {
            first: first_wall_second,
            last: machine.last().unwrap().wall_ns / NS,
        };
        let mut cohorts = Cohorts::with_room();
        let mut leaders = Leaders::new();

        leaders.choose(&readings, window, &mut cohorts);
        let sums = leaders.sums_over(&readings, window);

        (cohorts, sums)
    })
}

/// Thirteen blocks of two plain seconds: the first twelve name 24,000 Apps between them, each
/// second a thousand of its own, and the thirteenth names again the first second's Apps and
/// then one App more, which passes the bound on Apps of a history's means and of the leaders.
fn past_the_bound_on_apps(budget: &Arc<ByteBudget>) -> Vec<Vec<Arc<AppsSecond>>> {
    let keys = |first: usize, count: usize| -> Vec<Vec<u8>> {
        (first..first + count)
            .map(|index| format!("/App {index:05}.app").into_bytes())
            .collect()
    };
    let mut blocks: Vec<Vec<Arc<AppsSecond>>> = (0..12)
        .map(|block| {
            (0..2)
                .map(|index| {
                    let second = 2 * block + index;
                    let first = second * BOUND_APPS_PER_SECOND;
                    let keys = keys(first, BOUND_APPS_PER_SECOND);
                    Arc::new(apps_only(second, &keys, budget))
                })
                .collect()
        })
        .collect();
    blocks.push(vec![
        Arc::new(apps_only(24, &keys(0, BOUND_APPS_PER_SECOND), budget)),
        Arc::new(apps_only(25, &keys(24 * BOUND_APPS_PER_SECOND, 1), budget)),
    ]);

    blocks
}
