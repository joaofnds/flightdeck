use std::io::ErrorKind;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{Failure, Store, View};
use crate::live::identity_fixture::Fixture;

#[test]
fn a_sealed_sample_refusal_preserves_machine_observations_and_resets_rates() {
    // Goal: one refused process sample cannot erase its machine reading or adjacent samples.
    // Method: read a sealed normal/refused/normal minute directly, without the ring adapter.
    let mut fixture = Fixture::new_mode(true, 2_000, None);
    publish_named_processes(&mut fixture, 0, &[b"small"], 1);
    publish_named_processes(&mut fixture, 1, &[&vec![b'x'; 1_000]], 2_000);
    publish_named_processes(&mut fixture, 2, &[b"small"], 1);
    let ring = Store::start_with_payload(&fixture.root, 16 * 1024 * 1024).unwrap();
    let expected = view_where(&ring, |view| view.seconds.len() == 3);
    let refused =
        super::Missing::Refused(persistence::Error::Bound("live App sample bytes").to_string());
    assert_eq!(expected.apps[1].missing, Some(refused.clone()));
    drop(ring);
    crate::live::app_process_groups_history_tests::named::seal_selected(&fixture, &[1, 2, 3]);

    let actual = sealed_payload_view(&fixture);

    assert_eq!(actual.seconds, expected.seconds);
    assert_eq!(actual.apps, expected.apps);
    assert_eq!(actual.left_out, None);
    assert_eq!(actual.apps[1].processes.as_ref().unwrap_err(), &refused);
    assert_eq!(actual.apps[2].processes.as_ref().unwrap().previous, None);
    assert_eq!(actual.apps[2].elapsed_ns, None);
    assert_eq!(actual.apps[1].observation.unwrap().stamp.publication, 2);
}

#[test]
fn both_adapters_preserve_the_exact_observation_when_apps_exceed_the_bound() {
    // Goal: an App-count refusal belongs to its original publication, including before ingestion.
    // Method: construct the refused second from the ring sample, then compare sealed-only output.
    let mut fixture = Fixture::new_mode(true, 2_049, None);
    expand_program_dictionary(&mut fixture);
    let names: Vec<_> = (0..2_049).map(|index| format!("process-{index}")).collect();
    let names: Vec<_> = names.iter().map(String::as_bytes).collect();
    publish_named_processes(&mut fixture, 0, &names, 2_049);
    let mut ring = super::ring::Ring::new(fixture.root.clone());
    ring.follow().unwrap();
    ring.read(|_, _, apps| {
        let super::apps::TickApps::Derived(sample) = apps else {
            panic!("the App count fits Sample's bound");
        };
        let mut second = super::AppsSecond::with_room();
        second.take(super::apps::TickApps::Derived(sample), None, None);
        assert_eq!(second.missing, Some(super::Missing::TooMany));
        assert_eq!(second.observation, Some(sample.selected));
        true
    })
    .unwrap();
    let store = Store::start_with_payload(&fixture.root, 16 * 1024 * 1024).unwrap();
    let expected = view_where(&store, |view| view.seconds.len() == 1);
    crate::live::app_process_groups_history_tests::named::seal_selected(&fixture, &[1]);

    let actual = sealed_payload_view(&fixture);

    assert_eq!(actual.seconds, expected.seconds);
    assert_eq!(actual.apps, expected.apps);
    assert_eq!(
        actual.apps[0].processes.as_ref().unwrap_err(),
        &super::Missing::TooMany
    );
    assert_eq!(actual.apps[0].observation.unwrap().stamp.publication, 1);
}

pub(super) fn expand_program_dictionary(fixture: &mut Fixture) {
    use collector::recording::{RegisteredRun, Registry};
    use collector::ring::{DictionaryCapacity, Place, RingReader, Writer};
    let reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let identity = fixture.writer.identity().unwrap();
    let writer = Writer::create_with_identity(
        &Place {
            lock_path: fixture.root.join("many-names.lock"),
            name_prefix: format!("/fdmany{}", std::process::id()),
        },
        reader.shape(),
        DictionaryCapacity {
            entries: 2_050,
            arena_bytes: 128 * 1024,
        },
        &identity,
        0,
    )
    .unwrap();
    Registry {
        runs: vec![RegisteredRun {
            identity,
            ring_name: writer.name().to_owned(),
            collector_pid: 0,
        }],
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
}

fn sealed_payload_view(fixture: &Fixture) -> Arc<View> {
    let budget = Arc::new(super::ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::new();
    held.enable_payload(Arc::clone(&budget));
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    held.left_out =
        super::recording::load_payload(&fixture.root, &budget, &operation, |run, second, apps| {
            assert!(held.take(run, second, apps));
        })
        .unwrap()
        .left_out;
    let mailbox = super::Mailbox::new();
    held.publish(&mailbox);
    mailbox.take().unwrap()
}

#[test]
fn both_adapters_keep_distinct_nonzero_read_and_write_intervals() {
    // Goal: neither payload copier substitutes reads for writes or cumulative counters for deltas.
    // Method: publish unequal increments above 2^53 and assert each process and App value directly.
    let mut fixture = Fixture::new();
    let mut usage = collector::processes::Usage {
        awake_ns_at_start: 0,
        user_ns: 0,
        system_ns: 0,
        energy_nj: 0,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 11,
        footprint_bytes: 12,
        disk_read_bytes: 9_007_199_254_740_993,
        disk_written_bytes: 9_007_199_254_741_993,
    };
    fixture.publish_with_usage(100, &usage);
    usage.disk_read_bytes += 17;
    usage.disk_written_bytes += 43;
    fixture.publish_with_usage(100, &usage);
    let store = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let ring = view_where(&store, |view| view.seconds.len() == 2);
    fixture.seal();

    let sealed = sealed_payload_view(&fixture);

    assert_directional_payload(&ring);
    assert_directional_payload(&sealed);
}

fn assert_directional_payload(view: &View) {
    let payload = view.apps[1].processes.as_ref().unwrap();
    let process = payload.iter().find(|process| process.pid == 7).unwrap();
    assert_eq!(process.disk_read_bytes, crate::apps::Amount::Known(17));
    assert_eq!(process.disk_written_bytes, crate::apps::Amount::Known(43));
    let app = &payload.apps()[usize::try_from(process.app_index.unwrap()).unwrap()];
    assert_eq!(
        app.disk_read_bytes,
        crate::live_app_response::Measure {
            known: 17,
            measured_members: 1,
            unavailable_members: 0,
            overflowed: false,
        }
    );
    assert_eq!(
        app.disk_written_bytes,
        crate::live_app_response::Measure {
            known: 43,
            measured_members: 1,
            unavailable_members: 0,
            overflowed: false,
        }
    );
}

pub(super) fn publish_named_processes(
    fixture: &mut Fixture,
    tick: u64,
    names: &[&[u8]],
    count: i32,
) {
    use collector::ring::{
        IdentityObservation, IdentityPublication, ObservationTimes, ProcessCreation,
        ProcessIdentity, Readings, UsageAvailability,
    };
    let programs = publish_named_processes_programs(fixture, names);
    let processes: Vec<_> = (1..=count)
        .map(|pid| IdentityObservation {
            pid,
            usage: UsageAvailability::Refused,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: u64::try_from(pid).unwrap(),
                    pid_version: 1,
                },
                program: programs[usize::try_from(pid - 1).unwrap() % programs.len()],
            },
        })
        .collect();
    let memory = collector::machine::Memory::default();
    fixture
        .writer
        .publish_with_identity(
            &Readings {
                run_tick: tick,
                cpus: &[],
                memory: &memory,
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: u32::try_from(count).unwrap(),
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: collector::ring::Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: 100_000_000_000,
                continuous_ns: tick * 1_000_000_000,
                awake_ns: tick * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &processes,
                events: &[],
            },
        )
        .unwrap();
}

fn publish_named_processes_programs(
    fixture: &mut Fixture,
    names: &[&[u8]],
) -> Vec<collector::ring::ProgramReference> {
    names
        .iter()
        .map(|name| {
            let mut program = None;
            fixture
                .writer
                .intern_program(
                    &collector::ring::ProgramBytes {
                        executable_name: name,
                        app_path: None,
                    },
                    &mut program,
                )
                .unwrap();
            program.unwrap()
        })
        .collect()
}

#[test]
fn a_view_identifies_the_second_publication_and_its_unattributed_process() {
    // Goal: wall seconds do not identify publications or exclude unattributed processes.
    // Method: publish two legacy ticks in one second and select the exact second sibling.
    let mut fixture = Fixture::new_mode(false, 2, None);
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);

    let store = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let view = view_where(&store, |view| view.seconds.len() == 2);
    let alternatives = view.alternatives(100);
    assert_eq!(alternatives.len(), 2);
    assert_eq!(alternatives[1].stamp.publication, 2);
    let selected = view.observation(&alternatives[1]).unwrap();
    let processes = selected.processes.as_ref().unwrap();
    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].pid, 7);
    assert_eq!(processes[0].app_index, None);
    assert_eq!(selected.observation, Some(alternatives[1]));
}

#[test]
fn sealed_payloads_match_ring_payloads_including_refused_processes() {
    // Goal: sealing preserves every typed process and App measure, including refusal.
    // Method: retain a ring View and compare its two same-second ticks with the sealed adapter.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    let ring = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let expected = view_where(&ring, |view| view.seconds.len() == 2);
    fixture.seal();

    let actual = sealed_payload_view(&fixture);

    assert_eq!(actual.apps, expected.apps);
    let processes = actual.apps[1].processes.as_ref().unwrap();
    assert_eq!(processes.len(), 2);
    assert_eq!(processes[0].pid, 1);
    assert_eq!(
        processes[0].cpu_ns,
        crate::apps::Amount::Unavailable(crate::apps::Unavailable::Refused)
    );
}

#[test]
fn an_older_sample_keeps_its_identity_and_ranked_apps_without_a_recording() {
    // Goal: a thirty-minute-back hover uses resident machine readings and rankings together.
    // Method: advance a store directly, then inspect its sampled payload with no source reader.
    let mut held = super::Held::new();
    held.enable_payload(Arc::new(super::ByteBudget::new(1024 * 1024)));
    for second in 100..=1_900 {
        let reading = second_at(second);
        let mut apps = ranked_second(&reading);
        assert!(held.take(&RUN, &reading, super::apps::TickApps::Loaded(&mut apps)));
    }

    let view = published(&mut held);
    let sample = view
        .sampled
        .iter()
        .find(|sample| sample.machine.wall_ns == 100_000_000_000)
        .unwrap();

    assert_eq!(
        sample.observation,
        super::take_observation(&RUN, &second_at(100))
    );
    assert_eq!(
        sample.machine.publication,
        sample.observation.stamp.publication
    );
    assert_eq!(
        sample.ranks().unwrap(),
        super::SampledRanks::from(&ranked_second(&second_at(100)).ranks().unwrap())
    );
    let ranks = sample.ranks().unwrap();
    let entries: Vec<_> = ranks.metrics[3].entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].app.bytes, b"process");
    assert_eq!(entries[0].amount, 100);
    assert_eq!(entries[0].rate, Some(100.0));
    assert_eq!(entries[0].measured_members, 1);
    assert_eq!(entries[0].unavailable_members, 0);
}

fn ranked_second(second: &super::MachineSecond) -> super::AppsSecond {
    let mut apps = super::AppsSecond::with_room();
    apps.observation = Some(super::take_observation(&RUN, second));
    apps.missing = None;
    apps.elapsed_ns = Some(1_000_000_000);
    let measure = crate::live_app_response::Measure {
        known: second.publication,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let app = crate::live_app_response::App {
        key: crate::live_app_response::Key::new(
            crate::live_app_response::KeyKind::Executable,
            b"process".to_vec(),
        )
        .unwrap(),
        display_name: String::from("process"),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measure,
        energy_nj: measure,
        gpu_ns: measure,
        disk_read_bytes: measure,
        disk_written_bytes: measure,
        disk_io_bytes: measure,
        resident_bytes: measure,
        footprint_bytes: measure,
    };
    let name = Arc::new(
        crate::live_app_response::RankedApp::new(
            crate::live_app_response::KeyKind::Executable,
            b"process".to_vec(),
        )
        .unwrap(),
    );
    apps.apps.push(super::AppRow::new(&app, name));
    apps
}

#[test]
fn a_missed_tick_resets_process_rates_in_both_adapters() {
    // Goal: a skipped collector tick breaks process rates even with adjacent publications.
    // Method: skip one tick, then compare ring and recorded payloads at that observation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.skip_run_ticks(1);
    fixture.publish_with_system_rows(102);
    let ring = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let expected = view_where(&ring, |view| view.seconds.len() == 2);
    fixture.seal();

    let actual = sealed_payload_view(&fixture);

    assert_eq!(expected.apps[1].elapsed_ns, None);
    assert_eq!(actual.apps, expected.apps);
}

#[test]
fn an_exhausted_budget_reports_an_unavailable_complete_process_list() {
    // Goal: an over-budget observation never presents a truncated list as complete.
    // Method: give a two-process observation no retained bytes and inspect its failure.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let budget = Arc::new(super::ByteBudget::new(0));

    let store = Store::start_with_budget(&fixture.root, Arc::clone(&budget)).unwrap();
    let view = view_where(&store, |view| view.seconds.len() == 1);

    assert_eq!(view.apps[0].processes, Err(super::Missing::Budget));
    assert_eq!(budget.used(), 0);
    assert_eq!(view.apps[0].usage_refused, 1);
}

#[test]
fn a_view_retains_its_payload_and_byte_reservations_after_the_store_stops() {
    // Goal: releasing the producer cannot recycle or uncharge a retained reader's payload.
    // Method: stop the store with a View held, inspect its processes, then release the View.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let budget = Arc::new(super::ByteBudget::new(1024 * 1024));
    let store = Store::start_with_budget(&fixture.root, Arc::clone(&budget)).unwrap();
    let view = view_where(&store, |view| view.seconds.len() == 2);

    drop(store);

    let first = view.apps[0].processes.as_ref().unwrap();
    let second = view.apps[1].processes.as_ref().unwrap();
    assert_eq!(budget.used(), first.bytes() + second.bytes());
    assert!(std::ptr::eq(
        first[0].executable_name().unwrap(),
        second[0].executable_name().unwrap()
    ));
    assert_eq!(second[1].pid, 7);
    drop(view);
    assert_eq!(budget.used(), 0);
}

#[test]
fn an_exact_selection_rejects_a_sibling_with_different_clocks() {
    // Goal: publication identity cannot silently accept inconsistent recorded clocks.
    // Method: change only the selected sibling's awake clock before looking it up.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let view = view_where(&store, |view| view.seconds.len() == 1);
    let mut selected = view.alternatives(100)[0];
    selected.stamp.awake_ns = Some(99);

    assert!(view.observation(&selected).is_none());
}

#[test]
fn sealed_legacy_processes_remain_queryable_without_app_attribution() {
    // Goal: the process decoder retains rows that App parts cannot represent.
    // Method: seal two legacy ticks and compare their complete payloads with the ring.
    let mut fixture = Fixture::new_mode(false, 2, None);
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    let ring = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let expected = view_where(&ring, |view| view.seconds.len() == 2);
    fixture.seal();

    let actual = sealed_payload_view(&fixture);

    assert_eq!(actual.apps, expected.apps);
    assert_eq!(
        actual.apps[1].processes.as_ref().unwrap()[0].app_index,
        None
    );
}

#[test]
fn a_sealed_minute_keeps_process_rates_from_its_predecessor_minute() {
    // Goal: a loader boundary does not discard the preceding tick's process baseline.
    // Method: seal adjacent ticks in different minutes and compare with their ring payloads.
    let mut fixture = Fixture::new();
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(159);
    fixture.publish_with_system_rows(160);
    let ring = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let expected = view_where(&ring, |view| view.seconds.len() == 2);
    fixture.seal_minute(0);
    fixture.seal_minute(1);

    let actual = sealed_payload_view(&fixture);

    assert_eq!(actual.apps, expected.apps);
    assert_eq!(actual.apps[1].elapsed_ns, Some(1_000_000_000));
    assert_eq!(
        actual.apps[1].processes.as_ref().unwrap().previous,
        actual.apps[0]
            .observation
            .map(|observation| observation.stamp)
    );
}

#[test]
fn a_sampled_refusal_reserves_its_owned_reason() {
    // Goal: unavailable rankings cannot allocate an uncharged error message.
    // Method: sample a refused observation whose reason is larger than its row storage.
    let reading = second_at(100);
    let mut apps = ranked_second(&reading);
    apps.apps.clear();
    apps.missing = Some(super::Missing::Refused("x".repeat(16_384)));
    let budget = Arc::new(super::ByteBudget::new(1024 * 1024));
    let mut sample = super::SampledSecond::empty();

    sample.take(&reading, &apps, &budget);

    assert_eq!(sample.ranks().unwrap_err(), apps.missing.as_ref().unwrap());
    assert!(budget.used() >= size_of::<super::SampledSecond>() + 16_384);
}

#[test]
fn a_retained_older_sample_stays_immutable_after_its_tail_expires() {
    // Goal: recycling the sampled tail never changes a sample held by an older reader.
    // Method: retain just its first sample, advance past the tail, and inspect both Views.
    let mut held = super::Held::new();
    held.enable_payload(Arc::new(super::ByteBudget::new(4 * 1024 * 1024)));
    for second in 100..=1_900 {
        let reading = second_at(second);
        let mut apps = ranked_second(&reading);
        assert!(held.take(&RUN, &reading, super::apps::TickApps::Loaded(&mut apps)));
    }
    let view = published(&mut held);
    let sample = Arc::clone(&view.sampled[0]);
    drop(view);

    for second in 1_901..=4_000 {
        let reading = second_at(second);
        let mut apps = ranked_second(&reading);
        assert!(held.take(&RUN, &reading, super::apps::TickApps::Loaded(&mut apps)));
        drop(published(&mut held));
    }
    let view = published(&mut held);

    assert!(
        view.sampled
            .iter()
            .all(|sample| sample.machine.publication > 100)
    );
    assert_eq!(sample.machine.publication, 100);
    assert_eq!(
        sample.ranks().unwrap(),
        super::SampledRanks::from(&ranked_second(&second_at(100)).ranks().unwrap())
    );
}

#[test]
fn both_adapters_keep_gone_and_refused_processes_without_app_identity() {
    // Goal: identity-only process rows survive when neither process can join an App.
    // Method: publish unavailable rows, seal them, and compare typed payloads and reasons.
    let mut fixture = Fixture::new();
    publish_unavailable_processes(&mut fixture, 0);
    publish_unavailable_processes(&mut fixture, 1);
    let ring = Store::start_with_payload(&fixture.root, 1024 * 1024).unwrap();
    let expected = view_where(&ring, |view| view.seconds.len() == 2);
    fixture.seal();

    let actual = sealed_payload_view(&fixture);

    assert_eq!(actual.apps, expected.apps);
    let rows = actual.apps[1].processes.as_ref().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].app_index, None);
    assert_eq!(rows[1].app_index, None);
    assert_eq!(
        rows[0].cpu_ns,
        crate::apps::Amount::Unavailable(crate::apps::Unavailable::Gone)
    );
    assert_eq!(
        rows[1].cpu_ns,
        crate::apps::Amount::Unavailable(crate::apps::Unavailable::Refused)
    );
}

fn publish_unavailable_processes(fixture: &mut Fixture, tick: u64) {
    use collector::ring::{
        IdentityObservation, IdentityPublication, IdentityUnavailable, ObservationTimes,
        ProcessCreation, ProcessIdentity, Readings, UsageAvailability,
    };
    let processes = [
        IdentityObservation {
            pid: 1,
            usage: UsageAvailability::Gone,
            identity: ProcessIdentity::Unavailable {
                creation: Some(ProcessCreation {
                    unique_id: 1,
                    pid_version: 1,
                }),
                reason: IdentityUnavailable::Gone,
            },
        },
        IdentityObservation {
            pid: 7,
            usage: UsageAvailability::Refused,
            identity: ProcessIdentity::Unavailable {
                creation: Some(ProcessCreation {
                    unique_id: 7,
                    pid_version: 7,
                }),
                reason: IdentityUnavailable::Refused,
            },
        },
    ];
    let memory = collector::machine::Memory::default();
    let readings = Readings {
        run_tick: tick,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 1,
        processes_gone: 1,
        coalitions: &[],
        events: &[],
        dropped: collector::ring::Dropped::default(),
    };
    let times = ObservationTimes {
        wall_ns: 100_000_000_000,
        continuous_ns: tick * 1_000_000_000,
        awake_ns: tick * 1_000_000_000,
    };
    fixture
        .writer
        .publish_with_identity(
            &readings,
            Some(&times),
            &IdentityPublication {
                processes: &processes,
                events: &[],
            },
        )
        .unwrap();
}

/// The first View the store hands over that `holds` accepts, failing after ten seconds.
pub(super) fn view_where(store: &Store, holds: impl Fn(&View) -> bool) -> Arc<View> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(view) = store.take() {
            if holds(&view) {
                return view;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the store handed over no such View"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_store_holds_each_ticks_second_oldest_first() {
    // Goal: the store takes in every tick the ring holds when it starts. Method: publish three
    // ticks, start a store over the recording, and read its View that holds three seconds.
    let mut fixture = Fixture::new();
    for second in 100..103 {
        fixture.publish_with_system_rows(second);
    }

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 3);
    assert_eq!(wall_seconds_of(&view.seconds), [100, 101, 102]);
}

#[test]
fn a_tick_published_later_reaches_a_view() {
    // Goal: the store follows the ring as the collector publishes. Method: start a store over
    // one tick, wait for its View, publish a second tick, and wait for a View of both.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start(&fixture.root).unwrap();
    view_where(&store, |view| view.seconds.len() == 1);

    fixture.publish_with_system_rows(101);

    let view = view_where(&store, |view| view.seconds.len() == 2);
    assert_eq!(wall_seconds_of(&view.seconds), [100, 101]);
}

#[test]
fn a_second_carries_the_rates_since_the_tick_before_it() {
    // Goal: the store derives each second's rates from the run's previous tick. Method:
    // publish two consecutive ticks whose CPU was busy half the time between them.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 2);
    assert_eq!(view.seconds[1].cpu_percent, Some(50.0));
}

#[test]
fn a_second_ranks_its_apps_as_the_worker_ranks_them() {
    // Goal: the page draws a second's top Apps from the View, ranked by the App rule the
    // worker runs. Method: publish three ticks, then rank the newest through the worker's
    // session and through a store's View.
    let mut fixture = Fixture::new();
    for second in 100..103 {
        fixture.publish_with_system_rows(second);
    }
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.select(102, 0).unwrap();
    let expected = session.selected_ranks().unwrap();

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 3);
    assert_eq!(view.apps[2].ranks(), Ok(expected));
}

#[test]
fn seconds_in_a_row_hold_one_name_for_each_app() {
    // Goal: an App's name is held once for every second that names it, which keeps each
    // second's Apps small. Method: publish two ticks and compare the names their seconds hold.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 2);
    let (first, second) = (&view.apps[0].apps, &view.apps[1].apps);
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert!(Arc::ptr_eq(&first[0].name, &second[0].name));
    assert!(Arc::ptr_eq(&first[1].name, &second[1].name));
}

#[test]
fn each_view_has_a_higher_version_than_the_one_before() {
    // Goal: the main thread tells a new View from one it drew by its version, since the store
    // writes each View's buffer again. Method: take the View of one tick, publish another, and
    // compare the versions.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start(&fixture.root).unwrap();
    let first = view_where(&store, |view| view.seconds.len() == 1);

    fixture.publish_with_system_rows(101);

    let second = view_where(&store, |view| view.seconds.len() == 2);
    assert!(second.version > first.version);
}

mod when_the_collector_starts_again {
    use super::super::ring::{FOLLOW_STEPS, Ring};
    use super::{Fixture, Store, view_where, wall_seconds_of};

    #[test]
    fn the_store_follows_the_new_run() {
        // Goal: a collector that starts again is followed into its new run, after the seconds
        // of the run before. Method: publish a tick of one run, register a second run after
        // it, publish a tick there, and read the View that holds both.
        let mut first = Fixture::new();
        first.publish_with_system_rows(100);
        let store = Store::start(&first.root).unwrap();
        view_where(&store, |view| view.seconds.len() == 1);
        let mut second = Fixture::new_mode(false, 2, Some(&first));

        second.publish_with_system_rows(101);

        let view = view_where(&store, |view| view.seconds.len() == 2);
        assert_eq!(wall_seconds_of(&view.seconds), [100, 101]);
    }

    #[test]
    fn the_store_looks_for_the_new_run_once_its_ring_goes_quiet() {
        // Goal: a step reads the runs registry only once in ten, and only once the ring the
        // store follows published nothing since the last look, so a new run is followed at the
        // second look after its collector's predecessor stopped, within two seconds of steps.
        // Method: follow a run through the ring adapter and take its tick, register a second run
        // with a tick, and count the steps until the adapter hands that tick over.
        let mut first = Fixture::new();
        first.publish_with_system_rows(100);
        let mut ring = Ring::new(first.root.clone());
        ring.follow().unwrap();
        ring.read(|_, _, _| true).unwrap();
        let mut second = Fixture::new_mode(false, 2, Some(&first));
        second.publish_with_system_rows(101);

        let steps = (1..=2 * FOLLOW_STEPS + 1).find(|_| {
            ring.follow().unwrap();
            let mut taken = false;
            ring.read(|_, _, _| {
                taken = true;
                true
            })
            .unwrap();
            taken
        });

        assert_eq!(steps, Some(2 * FOLLOW_STEPS));
    }
}

mod while_the_store_follows_the_newest_run {
    use super::super::ring::{FOLLOW_STEPS, Ring};
    use super::Fixture;

    #[test]
    fn a_look_for_a_new_run_leaves_the_recording_identity_unread() {
        // Goal: a look for a new run reads the runs registry alone while the store follows the
        // newest, since the store checked the recording's identity when it began following, and
        // each file opened costs the store more than reading a tick. Method: follow a run through
        // the ring adapter, make the recording's identity unreadable, and step until the adapter
        // looks at the registry again.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(100);
        let mut ring = Ring::new(fixture.root.clone());
        ring.follow().unwrap();
        std::fs::remove_file(fixture.root.join("identity")).unwrap();

        let looks: Vec<_> = (0..FOLLOW_STEPS).map(|_| ring.follow()).collect();

        assert_eq!(looks, vec![Ok(()); usize::try_from(FOLLOW_STEPS).unwrap()]);
    }

    #[test]
    fn a_ring_that_keeps_publishing_leaves_the_registry_unread() {
        // Goal: while the ring the store follows publishes, a look for a new run reads no file,
        // since a run starts only once the collector before it stops, and a look costs the
        // store about 0.1 ms. Method: follow a run, make the registry unreadable, and step
        // through three looks with a tick published before each.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(100);
        let mut ring = Ring::new(fixture.root.clone());
        ring.follow().unwrap();
        std::fs::write(fixture.root.join("runs"), "unreadable").unwrap();

        let mut looks = Vec::new();
        for second in 101..104 {
            fixture.publish_with_system_rows(second);
            for _ in 0..FOLLOW_STEPS {
                ring.read(|_, _, _| true).unwrap();
                looks.push(ring.follow());
            }
        }

        assert_eq!(
            looks,
            vec![Ok(()); usize::try_from(3 * FOLLOW_STEPS).unwrap()]
        );
    }

    #[test]
    fn a_failed_look_is_tried_again_while_the_ring_publishes() {
        // Goal: a look that failed is tried again at the next look even while the ring
        // publishes, so a failure the View reports clears once the registry reads again.
        // Method: follow a run, fail a look on an unreadable registry while the ring is quiet,
        // restore the registry, publish a tick, and step to the next look.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(100);
        let mut ring = Ring::new(fixture.root.clone());
        ring.follow().unwrap();
        ring.read(|_, _, _| true).unwrap();
        let registry = std::fs::read(fixture.root.join("runs")).unwrap();
        std::fs::write(fixture.root.join("runs"), "unreadable").unwrap();
        let failed = (0..2 * FOLLOW_STEPS).map(|_| ring.follow()).last().unwrap();
        std::fs::write(fixture.root.join("runs"), registry).unwrap();
        fixture.publish_with_system_rows(101);
        ring.read(|_, _, _| true).unwrap();

        let looked = (0..FOLLOW_STEPS).map(|_| ring.follow()).last().unwrap();

        assert!(failed.is_err());
        assert_eq!(looked, Ok(()));
    }
}

mod when_the_folder_holds_no_recording {
    use super::{ErrorKind, Failure, Store, view_where};

    #[test]
    fn the_view_reports_the_missing_recording() {
        // Goal: a window over a folder the collector never recorded into says why it shows no
        // readings. Method: start a store over a folder that does not exist.
        let root =
            std::env::temp_dir().join(format!("flightdeck-store-missing-{}", std::process::id()));

        let store = Store::start(&root).unwrap();

        let view = view_where(&store, |view| view.failure.is_some());
        assert_eq!(view.failure, Some(Failure::Recording(ErrorKind::NotFound)));
    }
}

mod when_the_live_span_lets_go_of_a_second {
    use super::super::{EARLIER_MAX, Held, SECONDS_MAX};
    use super::{RUN, UNREAD, published, second_at, wall_seconds_of};

    #[test]
    fn the_store_keeps_the_first_second_of_each_ten_second_stretch() {
        // Goal: a chart of the loaded hour draws one reading in every ten seconds past the live
        // span, as the older tail draws them. Method: take in 25 seconds more than the live span
        // holds, from a wall second that starts no stretch, and read the older seconds.
        let mut held = Held::new();
        let first = 1_005;
        let last = first + u64::try_from(SECONDS_MAX).unwrap() + 25;

        for wall_second in first..last {
            assert!(held.take(&RUN, &second_at(wall_second), UNREAD));
        }

        let view = published(&mut held);
        assert_eq!(wall_seconds_of(&view.earlier), [1_005, 1_010, 1_020]);
    }

    #[test]
    fn the_store_lets_go_of_the_oldest_stretch_past_the_loaded_hour() {
        // Goal: the older seconds stay within their bound. Method: let the live span go of one
        // stretch more than the older seconds hold, and read the oldest of them.
        let mut held = Held::new();
        let let_go = (u64::try_from(EARLIER_MAX).unwrap() + 1) * 10;
        let last = 1_000 + u64::try_from(SECONDS_MAX).unwrap() + let_go;

        for wall_second in 1_000..last {
            assert!(held.take(&RUN, &second_at(wall_second), UNREAD));
        }

        let view = published(&mut held);
        assert_eq!(view.earlier.len(), EARLIER_MAX);
        assert_eq!(wall_seconds_of(&view.earlier[..1]), [1_010]);
    }
}

mod when_a_second_reaches_the_store_twice {
    use super::super::Held;
    use super::{RUN, UNREAD, published, second_at, wall_seconds_of};
    use collector::ring::{Identity, RunIdentity};

    #[test]
    fn the_store_holds_it_once() {
        // Goal: a tick the recording sealed and the ring still holds shows once. Method: take in
        // one run's second, then that second and the one before it again.
        let mut held = Held::new();
        assert!(held.take(&RUN, &second_at(101), UNREAD));

        assert!(held.take(&RUN, &second_at(100), UNREAD));
        assert!(held.take(&RUN, &second_at(101), UNREAD));

        let view = published(&mut held);
        assert_eq!(wall_seconds_of(&view.seconds), [101]);
    }

    #[test]
    fn a_second_of_the_next_run_follows_it() {
        // Goal: a collector that starts again numbers its publications afresh, so the store
        // tells its seconds from the run's before by the run. Method: take in a second of one
        // run, then a second of another run with a lower publication.
        let mut held = Held::new();
        let next = RunIdentity {
            run: Identity([4, 4]),
            ..RUN
        };
        assert!(held.take(&RUN, &second_at(101), UNREAD));

        assert!(held.take(&next, &second_at(5), UNREAD));

        let view = published(&mut held);
        assert_eq!(wall_seconds_of(&view.seconds), [101, 5]);
    }
}

mod when_the_readings_break {
    use super::super::{Held, SECONDS_MAX};
    use super::{RUN, UNREAD, published, second_at};
    use collector::ring::{Identity, RunIdentity};

    #[test]
    fn a_second_that_does_not_follow_the_one_before_starts_a_segment() {
        // Goal: a chart breaks its line where the readings break. Method: take in two pairs of
        // seconds, each second of a pair following the one before it, and read their segments.
        let mut held = Held::new();

        for (wall_second, follows) in [(100, false), (101, true), (102, false), (103, true)] {
            assert!(held.take(&RUN, &following(wall_second, follows), UNREAD));
        }

        assert_eq!(segments_of(&published(&mut held).seconds), [0, 0, 1, 1]);
    }

    #[test]
    fn a_second_of_another_run_starts_a_segment() {
        // Goal: a line never joins the readings of two runs. Method: take in a second of the
        // next run that follows a second of its own run, after a second of this run.
        let mut held = Held::new();
        let next = RunIdentity {
            run: Identity([4, 4]),
            ..RUN
        };

        assert!(held.take(&RUN, &following(100, false), UNREAD));
        assert!(held.take(&next, &following(5, true), UNREAD));

        assert_eq!(segments_of(&published(&mut held).seconds), [0, 1]);
    }

    #[test]
    fn an_older_second_keeps_the_segment_its_readings_had() {
        // Goal: the older seconds, one in ten, break their line where the readings between them
        // broke. Method: break the readings at a second the older seconds leave out, let the
        // live span go of three stretches, and read the older seconds' segments.
        let mut held = Held::new();
        let last = 1_030 + u64::try_from(SECONDS_MAX).unwrap();

        for wall_second in 1_000..last {
            assert!(held.take(&RUN, &following(wall_second, wall_second != 1_015), UNREAD));
        }

        assert_eq!(segments_of(&published(&mut held).earlier), [0, 0, 1]);
    }

    fn following(wall_second: u64, follows: bool) -> super::super::MachineSecond {
        super::super::MachineSecond {
            follows,
            ..second_at(wall_second)
        }
    }

    fn segments_of(seconds: &[std::sync::Arc<super::super::MachineSecond>]) -> Vec<u64> {
        seconds.iter().map(|second| second.segment).collect()
    }
}

mod when_the_app_rule_refuses_a_tick {
    use super::super::apps::TickApps;
    use super::super::{Held, MachineSecond};
    use super::{RUN, published, second_at};
    use crate::apps::Stamp;
    use crate::live_app_response::Observation;

    #[test]
    fn its_second_holds_the_tick_it_holds_no_apps_of() {
        // Goal: a chart places a second whose Apps the App rule refused, and breaks its line
        // around it, as the worker's App histories do. Method: take in a refused tick and read
        // the tick its second names.
        let mut held = Held::new();
        let second = MachineSecond {
            continuous_ns: Some(7_000_000_000),
            awake_ns: Some(6_000_000_000),
            ..second_at(100)
        };

        assert!(held.take(&RUN, &second, TickApps::Refused("too many members")));

        let tick = Observation {
            recording: [1, 1],
            stamp: Stamp {
                boot: [3, 3],
                run: [2, 2],
                publication: 100,
                continuous_ns: Some(7_000_000_000),
                awake_ns: Some(6_000_000_000),
            },
            run_tick: 100,
            wall_ns: Some(100_000_000_000),
        };
        assert_eq!(published(&mut held).apps[0].observation, Some(tick));
    }
}

mod when_a_tick_is_missed {
    use super::super::{Held, MachineSecond};
    use super::{RUN, UNREAD, published, second_at};

    #[test]
    fn the_app_charts_break_their_line_after_it() {
        // Goal: an App chart breaks its line where the worker's App histories break it, before
        // a second whose tick is not the one after the tick before it. Method: take in seconds
        // 100, 101 and 103, each with the clocks the rule compares, and read which follow.
        let mut held = Held::new();

        for wall_second in [100, 101, 103] {
            assert!(held.take(&RUN, &clocked(wall_second), UNREAD));
        }

        let view = published(&mut held);
        let consecutive: Vec<bool> = view.apps.iter().map(|second| second.consecutive).collect();
        assert_eq!(consecutive, [false, true, false]);
    }

    /// The second at `wall_second`, its continuous and awake clocks that many seconds in.
    fn clocked(wall_second: u64) -> MachineSecond {
        MachineSecond {
            continuous_ns: Some(wall_second * 1_000_000_000),
            awake_ns: Some(wall_second * 1_000_000_000),
            ..second_at(wall_second)
        }
    }
}

/// What a second the tests take in directly holds of its Apps.
const UNREAD: super::apps::TickApps<'static> =
    super::apps::TickApps::Missing(&super::Missing::Unread);

/// A run of a recording, as the tests that take seconds in directly name it.
const RUN: collector::ring::RunIdentity = collector::ring::RunIdentity {
    recording: collector::ring::Identity([1, 1]),
    run: collector::ring::Identity([2, 2]),
    boot: collector::ring::Identity([3, 3]),
};

/// The second at whole wall second `wall_second`, which is also its publication.
fn second_at(wall_second: u64) -> super::MachineSecond {
    super::MachineSecond {
        publication: wall_second,
        run_tick: wall_second,
        wall_ns: wall_second * 1_000_000_000,
        ..super::MachineSecond::default()
    }
}

/// The View `held` hands over next.
fn published(held: &mut super::Held) -> Arc<View> {
    let mailbox = super::Mailbox::new();
    held.publish(&mailbox);
    mailbox.take().unwrap()
}

/// The whole wall-clock second of each of `seconds`.
fn wall_seconds_of(seconds: &[Arc<super::MachineSecond>]) -> Vec<u64> {
    seconds
        .iter()
        .map(|second| second.wall_ns / 1_000_000_000)
        .collect()
}

mod when_the_recording_holds_sealed_minutes {
    use std::sync::Arc;

    use super::{Failure, Fixture, Store, View, held_through, view_where, wall_seconds_of};
    use crate::store::{MachineSecond, Missing};

    #[test]
    fn the_store_holds_the_sealed_seconds_the_ring_no_longer_holds() {
        // Goal: a window that opens draws the hour before the ring's newest ticks. Method: seal
        // a minute of three ticks, publish the next minute's ticks until the ring holds none of
        // the sealed ones, and start a store.
        let mut fixture = Fixture::new();
        for second in 100..103 {
            fixture.publish_with_system_rows(second);
        }
        fixture.seal_minute(0);
        fixture.skip_run_ticks(57);
        for second in 160..168 {
            fixture.publish_with_system_rows(second);
        }

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 167));
        let mut expected = vec![100, 101, 102];
        expected.extend(160..168);
        assert_eq!(wall_seconds_of(&view.seconds), expected);
    }

    #[test]
    fn a_store_built_from_the_recording_holds_what_one_built_from_the_ring_holds() {
        // Goal: the ring and the recording hand the store the same seconds, the rates of a
        // sealed minute's first second included. Method: follow two minutes' ticks with a store
        // over the ring, seal both, publish until the ring holds neither, start a second store,
        // and compare the seconds the two hold.
        let mut fixture = Fixture::new();
        fixture.skip_run_ticks(57);
        let from_ring = Store::start(&fixture.root).unwrap();
        for second in 157..162 {
            fixture.publish_with_system_rows(second);
            view_where(&from_ring, |view| held_through(view, second));
        }
        fixture.seal_minute(0);
        fixture.seal_minute(1);
        fixture.skip_run_ticks(58);
        for second in 240..248 {
            fixture.publish_with_system_rows(second);
        }

        let from_recording = Store::start(&fixture.root).unwrap();

        let expected = view_where(&from_ring, |view| held_through(view, 247));
        let view = view_where(&from_recording, |view| held_through(view, 247));
        assert_eq!(seconds_of(&view), seconds_of(&expected));
        assert_eq!(view.seconds[3].cpu_percent, Some(50.0));
    }

    #[test]
    fn a_store_built_from_the_recording_holds_the_apps_one_built_from_the_ring_holds() {
        // Goal: a sealed second holds the Apps its tick gave a store that read it from the ring,
        // so the page draws the same Apps whichever way the store took the second in. Method:
        // follow two minutes' ticks with a store over the ring, seal both with their Apps,
        // publish until the ring holds only the newest sealed tick, as the collector's two-minute
        // ring holds ticks it sealed, start a second store, and compare the Apps the two hold.
        let mut fixture = Fixture::new();
        fixture.skip_run_ticks(57);
        let from_ring = Store::start(&fixture.root).unwrap();
        for second in 157..162 {
            fixture.publish_with_system_rows(second);
            view_where(&from_ring, |view| held_through(view, second));
        }
        let derive = persistence::sealed_apps::SealedApps::default();
        fixture.seal_minute_derived(0, &derive);
        fixture.seal_minute_derived(1, &derive);
        fixture.skip_run_ticks(58);
        for second in 240..247 {
            fixture.publish_with_system_rows(second);
        }

        let from_recording = Store::start(&fixture.root).unwrap();

        let expected = view_where(&from_ring, |view| held_through(view, 246));
        let view = view_where(&from_recording, |view| held_through(view, 246));
        assert_eq!(view.apps, expected.apps);
        assert_eq!(view.apps[3].missing, None);
        assert!(Arc::ptr_eq(
            &view.apps[2].apps[0].name,
            &view.apps[3].apps[0].name
        ));
    }

    #[test]
    fn a_sealed_second_older_than_the_live_span_holds_no_apps() {
        // Goal: the store reads the Apps of the 18 minutes a live chart draws and none older,
        // which keeps its launch short and its memory small. Method: seal a minute, seal another
        // more than 18 minutes later, both with their Apps, publish until the ring holds neither,
        // and start a store.
        let mut fixture = Fixture::new();
        let derive = persistence::sealed_apps::SealedApps::default();
        fixture.publish_with_system_rows(100);
        fixture.seal_minute_derived(0, &derive);
        fixture.skip_run_ticks(59);
        fixture.publish_with_system_rows(1_300);
        fixture.seal_minute_derived(1, &derive);
        fixture.skip_run_ticks(59);
        for second in 1_400..1_408 {
            fixture.publish_with_system_rows(second);
        }

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 1_407));
        assert_eq!(wall_seconds_of(&view.seconds)[..2], [100, 1_300]);
        assert_eq!(view.apps[0].missing, Some(Missing::Unread));
        assert_eq!(view.apps[1].missing, None);
    }

    #[test]
    fn a_minute_older_than_the_loaded_hour_is_left_out() {
        // Goal: the store reads the hour a chart reaches back and nothing before it. Method: seal
        // a minute, seal another more than the hour and its lead-in later, publish until the
        // ring holds neither, and start a store.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(100);
        fixture.seal_minute(0);
        fixture.skip_run_ticks(59);
        fixture.publish_with_system_rows(3_800);
        fixture.seal_minute(1);
        fixture.skip_run_ticks(59);
        for second in 3_900..3_908 {
            fixture.publish_with_system_rows(second);
        }

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 3_907));
        let mut expected = vec![3_800];
        expected.extend(3_900..3_908);
        assert_eq!(wall_seconds_of(&view.seconds), expected);
    }

    #[test]
    fn a_minute_that_does_not_verify_leaves_a_gap_the_view_reports() {
        // Goal: a sealed minute whose files changed is held as missing, never as readings.
        // Method: seal two minutes, empty one file of the first, publish until the ring holds
        // neither, and start a store.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(100);
        fixture.seal_minute(0);
        fixture.skip_run_ticks(59);
        fixture.publish_with_system_rows(160);
        fixture.seal_minute(1);
        fixture.skip_run_ticks(59);
        for second in 220..228 {
            fixture.publish_with_system_rows(second);
        }
        empty_ticks_file(&fixture, 0);

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 227));
        let mut expected = vec![160];
        expected.extend(220..228);
        assert_eq!(wall_seconds_of(&view.seconds), expected);
        assert_eq!(view.left_out, Some(Failure::Refused));
    }

    #[test]
    fn each_loader_decodes_about_an_even_share_of_the_hour() {
        // Goal: the minutes whose Apps the store reads, the newest, do not fall on one loader,
        // since the hour is held once the slowest loader is done. Method: split the loaded
        // hour's 44 older minutes, costing one each, and the live span's 18, costing three,
        // and compare each batch's cost with an even share of 17.
        let mut costs = vec![1; 44];
        costs.extend([3; 18]);

        let batches = crate::store::recording::load_batches_split(&costs);

        let minutes: Vec<usize> = batches.iter().flat_map(Clone::clone).collect();
        assert_eq!(minutes, (0..62).collect::<Vec<_>>());
        for batch in batches {
            assert!(costs[batch].iter().sum::<usize>() < 17 + 3);
        }
    }

    fn seconds_of(view: &View) -> Vec<MachineSecond> {
        view.seconds.iter().map(|second| **second).collect()
    }

    /// Empties the ticks table of the fixture's sealed minute `minute`.
    fn empty_ticks_file(fixture: &Fixture, minute: u64) {
        let store = persistence::Store::open(&fixture.root).unwrap();
        let marker = store.marker_path(&persistence::WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window: minute,
        });
        let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
        let ticks = fixture
            .root
            .join(&manifest.data_directory)
            .join("ticks.parquet");
        std::fs::write(ticks, []).unwrap();
    }
}

mod when_the_seconds_hold_more_apps_than_a_chart_draws {
    use std::sync::Arc;

    use collector::processes::Usage;
    use serde_json::Value;

    use super::super::{
        Attribution, AttributionReading, COHORT_MAX, Cohorts, OtherReading, Resident,
        ResidentOther, View,
    };
    use super::{Fixture, Store, view_where};
    use crate::live::app_process_groups_history_tests::named;
    use crate::live_app_response::{Measure, RankedApp};
    use crate::{AppAttributionMetric, LiveSession};

    const APPS: [&[u8]; 10] = [
        b"/A.app", b"/B.app", b"/C.app", b"/D.app", b"/E.app", b"/F.app", b"/G.app", b"/H.app",
        b"/I.app", b"/J.app",
    ];

    /// The thousands of bytes each of `APPS` reads in each second, E and F the same.
    const DISK_THOUSANDS: [u64; 10] = [5, 3, 9, 1, 7, 7, 2, 8, 4, 6];

    /// The kibibytes each of `APPS` holds resident at each second, C and G the same.
    const RESIDENT_KIBIBYTES: [u64; 10] = [4, 9, 6, 2, 8, 1, 6, 3, 7, 5];

    /// The CPU time A uses in its second second on top of its own step, which makes it the App
    /// that used the most CPU over the seconds only while the chart draws that second.
    const BURST_NS: u64 = 40_000;

    /// The Apps each App chart draws on a line of its own, by name, the most first.
    #[derive(Debug, PartialEq)]
    struct Chosen {
        cpu: Vec<String>,
        power: Vec<String>,
        disk: Vec<String>,
        resident: Vec<String>,
    }

    /// What each App chart shows at each second it draws, oldest first.
    #[derive(Debug, PartialEq)]
    struct Shown {
        cpu: Vec<Attribution>,
        power: Vec<Attribution>,
        disk: Vec<Attribution>,
        resident: Vec<Resident>,
    }

    #[test]
    fn the_store_chooses_the_apps_the_worker_chooses_for_each_chart() {
        // Goal: Overview's App charts drawn from the store draw on lines of their own the Apps
        // the worker chooses, the eight that used the most of each resource over the seconds
        // the charts draw, or held the most resident memory on average over them, ties going
        // to the App first in key order. Method: ten Apps over five seconds, the first using
        // the most CPU only in its second second, two of them reading the same bytes and two
        // holding the same memory, followed second by second by the worker and by a store, and
        // the Apps each chooses for each chart compared.
        let (_fixture, session, view) = followed(rows, 5);

        let worker = chosen_by_worker(&session);

        assert_eq!(
            worker,
            Chosen {
                cpu: names_of(["A", "J", "I", "H", "G", "F", "E", "D"]),
                power: names_of(["A", "B", "C", "D", "E", "F", "G", "H"]),
                disk: names_of(["C", "H", "E", "F", "J", "A", "I", "B"]),
                resident: names_of(["B", "E", "I", "C", "G", "J", "A", "H"]),
            }
        );
        assert_eq!(chosen_by_store(&view.cohorts), worker);
    }

    #[test]
    fn each_second_shows_the_readings_the_worker_shows_on_each_chart() {
        // Goal: each second of Overview's App charts drawn from the store shows what the
        // worker's App histories show there, each chosen App's rate and measure, or its
        // resident memory, and the other Apps added up. Method: the ten Apps over five
        // seconds, followed by the worker and by a store, and each second of each chart
        // compared.
        let (_fixture, session, view) = followed(rows, 5);

        let shown = shown_by_store(&view);

        assert_eq!(shown, shown_by_worker(&session));
    }

    #[test]
    fn a_chart_ranks_its_apps_over_only_the_seconds_it_draws() {
        // Goal: an App that used the most only in seconds older than the charts draw no longer
        // leads them, as it no longer leads the worker's. Method: A uses five seconds of CPU in
        // the second second and none after, and holds the most memory until then, B a little
        // CPU every second and the same memory throughout, and the newest seconds come sixteen
        // minutes later, so the seconds A led lie before those the charts draw.
        let mut fixture = named::dense_with_system();
        named::publish(&mut fixture, 1, &burst_rows(1));
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let store = Store::start(&fixture.root).unwrap();
        let mut view = view_where(&store, |view| view.seconds.len() == 1);

        for publication in [2, 962, 963] {
            named::publish(&mut fixture, publication, &burst_rows(publication));
            session.poll().unwrap();
            view = view_where(&store, |view| {
                super::held_through(view, 1_000 + publication)
            });
        }
        session.select(1_963, 0).unwrap();
        let cpu = chosen_by_worker_of(&session, AppAttributionMetric::Cpu);
        let resident = chosen_by_worker_of_resident(&session);

        assert_eq!(cpu, ["/B.app", "/A.app"]);
        assert_eq!(chosen_by_store_of(&view.cohorts.cpu), cpu);
        assert_eq!(resident, ["/B.app", "/A.app"]);
        assert_eq!(chosen_by_store_of(&view.cohorts.resident), resident);
    }

    mod when_a_chart_draws_every_app {
        use super::{burst_rows, followed, shown_by_store, shown_by_worker};

        #[test]
        fn the_other_apps_show_what_the_worker_shows() {
            // Goal: a chart that draws every App of a second on a line of its own shows the
            // other Apps as the worker shows them, unknown at a second the ring holds, since
            // the live App rule never finds such a second's members complete. Method: A and B
            // over three seconds, which every chart draws, followed by the worker and by a
            // store, and each second compared.
            let (_fixture, session, view) = followed(burst_rows, 3);

            let shown = shown_by_store(&view);

            assert_eq!(shown, shown_by_worker(&session));
        }
    }

    mod when_an_app_holds_no_memory_at_a_second {
        use super::{exit_rows, followed, shown_by_store, shown_by_worker};

        #[test]
        fn the_charts_leave_it_out_as_the_worker_does() {
            // Goal: an App whose processes all exited before a tick holds no resident memory
            // there, so the chart of resident memory neither draws it there nor counts it among
            // the other Apps, as the worker's does. Method: A and B over three seconds, C at the
            // first two, its one process exited at the second, and D at the third only, its one
            // process exited, followed by the worker and by a store, and each second of each
            // chart compared.
            let (_fixture, session, view) = followed(exit_rows, 3);

            let shown = shown_by_store(&view);

            assert_eq!(shown, shown_by_worker(&session));
        }
    }

    mod when_the_clock_is_set_back {
        use super::super::{Store, held_through, view_where};
        use super::{burst_rows, chosen_by_store_of, named};

        #[test]
        fn the_charts_rank_their_apps_over_the_seconds_since() {
            // Goal: seconds the clock was set back past sit before the newest seconds in the
            // span but after them in time, so the charts rank their Apps over the seconds after
            // the clock went back. Method: A uses five seconds of CPU in the second second and
            // B a little every second, then a third second whose clock reads two minutes
            // earlier.
            let mut fixture = named::dense_with_system();
            let store = Store::start(&fixture.root).unwrap();
            for publication in 1..=2 {
                named::publish(&mut fixture, publication, &burst_rows(publication));
                view_where(&store, |view| held_through(view, 1_000 + publication));
            }

            named::publish_at(
                &mut fixture,
                3,
                &burst_rows(3),
                &collector::ring::ObservationTimes {
                    wall_ns: 880_000_000_000,
                    continuous_ns: 3_000_000_000,
                    awake_ns: 3_000_000_000,
                },
            );

            let view = view_where(&store, |view| held_through(view, 880));
            assert_eq!(chosen_by_store_of(&view.cohorts.cpu), ["/B.app", "/A.app"]);
        }
    }

    /// A recording of the Apps `rows_at` names at each publication from the first to `last`,
    /// followed second by second by a worker that selects the newest and by a store, with the
    /// store's View of them.
    fn followed(
        rows_at: fn(u64) -> Vec<named::Named<'static>>,
        last: u64,
    ) -> (Fixture, LiveSession, Arc<View>) {
        let mut fixture = named::dense_with_system();
        named::publish(&mut fixture, 1, &rows_at(1));
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let store = Store::start(&fixture.root).unwrap();
        let mut view = view_where(&store, |view| view.seconds.len() == 1);

        for publication in 2..=last {
            named::publish(&mut fixture, publication, &rows_at(publication));
            session.poll().unwrap();
            let count = usize::try_from(publication).unwrap();
            view = view_where(&store, |view| view.seconds.len() == count);
        }
        session.select(1_000 + last, 0).unwrap();

        (fixture, session, view)
    }

    /// A and B at `publication`, A having used five seconds of CPU at the second second and
    /// none since, and B a microsecond each second. A holds eight kibibytes resident up to the
    /// second second and one since, and B two throughout.
    fn burst_rows(publication: u64) -> Vec<named::Named<'static>> {
        let burst_ns = match publication {
            2.. => 5_000_000_000,
            _ => 0,
        };
        let resident_kibibytes = match publication {
            ..=2 => 8,
            _ => 1,
        };
        let a = Usage {
            resident_bytes: resident_kibibytes * 1_024,
            ..usage_of(burst_ns)
        };
        let b = Usage {
            resident_bytes: 2 * 1_024,
            ..usage_of(1_000 * publication)
        };

        vec![
            named::member(10, b"worker", b"/A.app", a),
            named::member(11, b"worker", b"/B.app", b),
        ]
    }

    /// A and B at `publication` as `burst_rows` has them, C at the first second and at the
    /// second, whose one process exited before the second's tick, and D at the third only, its
    /// one process exited too. A tick of the fixture's ring holds one exit.
    fn exit_rows(publication: u64) -> Vec<named::Named<'static>> {
        let mut rows = burst_rows(publication);
        if publication <= 2 {
            let mut row = named::member(12, b"worker", b"/C.app", usage_of(2_000 * publication));
            row.exited = publication == 2;
            rows.push(row);
        }
        if publication == 3 {
            let mut row = named::member(13, b"worker", b"/D.app", usage_of(3_000));
            row.exited = true;
            rows.push(row);
        }

        rows
    }

    /// Each of `APPS` at `publication`.
    fn rows(publication: u64) -> Vec<named::Named<'static>> {
        APPS.iter()
            .enumerate()
            .map(|(index, app)| {
                let pid = 10 + i32::try_from(index).unwrap();
                named::member(pid, b"worker", app, usage(index, publication))
            })
            .collect()
    }

    /// What App `index` of `APPS` has used by `publication`, a step of each resource a second.
    fn usage(index: usize, publication: u64) -> Usage {
        let place = u64::try_from(index).unwrap();
        let burst_ns = match (index, publication) {
            (0, 2..) => BURST_NS,
            _ => 0,
        };

        Usage {
            energy_nj: (10 - place) * 1_000 * publication,
            disk_read_bytes: DISK_THOUSANDS[index] * 1_000 * publication,
            resident_bytes: RESIDENT_KIBIBYTES[index] * 1_024,
            ..usage_of((place + 1) * 1_000 * publication + burst_ns)
        }
    }

    /// What a process has used by a second where it has used `user_ns` of CPU and nothing else
    /// a chart draws.
    fn usage_of(user_ns: u64) -> Usage {
        Usage {
            awake_ns_at_start: 0,
            user_ns,
            system_ns: 0,
            energy_nj: 0,
            instructions: 0,
            super_instructions: 0,
            cycles: 0,
            idle_wakeups: 0,
            interrupt_wakeups: 0,
            pageins: 0,
            resident_bytes: 1024,
            footprint_bytes: 2048,
            disk_read_bytes: 0,
            disk_written_bytes: 0,
        }
    }

    fn names_of(letters: [&str; 8]) -> Vec<String> {
        letters
            .iter()
            .map(|letter| format!("/{letter}.app"))
            .collect()
    }

    fn chosen_by_worker(session: &LiveSession) -> Chosen {
        Chosen {
            cpu: chosen_by_worker_of(session, AppAttributionMetric::Cpu),
            power: chosen_by_worker_of(session, AppAttributionMetric::Power),
            disk: chosen_by_worker_of(session, AppAttributionMetric::Disk),
            resident: chosen_by_worker_of_resident(session),
        }
    }

    fn chosen_by_worker_of(session: &LiveSession, metric: AppAttributionMetric) -> Vec<String> {
        chosen_by_worker_cohort(&session.app_attribution_history(metric).unwrap())
    }

    fn chosen_by_worker_of_resident(session: &LiveSession) -> Vec<String> {
        chosen_by_worker_cohort(&session.app_resident_history().unwrap())
    }

    fn chosen_by_worker_cohort(response: &Value) -> Vec<String> {
        response["cohort"]
            .as_array()
            .unwrap()
            .iter()
            .map(|app| app["key"]["display"].as_str().unwrap().to_owned())
            .collect()
    }

    fn chosen_by_store(cohorts: &Cohorts) -> Chosen {
        Chosen {
            cpu: chosen_by_store_of(&cohorts.cpu),
            power: chosen_by_store_of(&cohorts.power),
            disk: chosen_by_store_of(&cohorts.disk),
            resident: chosen_by_store_of(&cohorts.resident),
        }
    }

    fn chosen_by_store_of(cohort: &[Arc<RankedApp>]) -> Vec<String> {
        cohort
            .iter()
            .map(|app| String::from_utf8(app.bytes.clone()).unwrap())
            .collect()
    }

    fn shown_by_store(view: &View) -> Shown {
        Shown {
            cpu: shown_by_store_of(view, AppAttributionMetric::Cpu),
            power: shown_by_store_of(view, AppAttributionMetric::Power),
            disk: shown_by_store_of(view, AppAttributionMetric::Disk),
            resident: shown_by_store_of_resident(view),
        }
    }

    fn shown_by_store_of(view: &View, metric: AppAttributionMetric) -> Vec<Attribution> {
        let cohort = view.cohorts.of(metric).unwrap();
        let drawn = &view.apps[view.apps.len() - view.cohorts.seconds..];

        drawn
            .iter()
            .map(|second| second.attribution(metric, cohort))
            .collect()
    }

    fn shown_by_store_of_resident(view: &View) -> Vec<Resident> {
        let drawn = &view.apps[view.apps.len() - view.cohorts.seconds..];

        drawn
            .iter()
            .map(|second| second.resident(&view.cohorts.resident))
            .collect()
    }

    fn shown_by_worker(session: &LiveSession) -> Shown {
        Shown {
            cpu: shown_by_worker_of(session, AppAttributionMetric::Cpu),
            power: shown_by_worker_of(session, AppAttributionMetric::Power),
            disk: shown_by_worker_of(session, AppAttributionMetric::Disk),
            resident: shown_by_worker_of_resident(session),
        }
    }

    fn shown_by_worker_of(session: &LiveSession, metric: AppAttributionMetric) -> Vec<Attribution> {
        let response = session.app_attribution_history(metric).unwrap();
        response["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| attribution_of(point, metric))
            .collect()
    }

    fn shown_by_worker_of_resident(session: &LiveSession) -> Vec<Resident> {
        let response = session.app_resident_history().unwrap();
        response["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(resident_of)
            .collect()
    }

    /// What the worker's `point` shows on the chart of resident memory by App.
    fn resident_of(point: &Value) -> Resident {
        let mut resident = Resident {
            apps: [None; COHORT_MAX],
            other: None,
        };
        let apps = point["apps"].as_array().unwrap();
        for (measure, app) in resident.apps.iter_mut().zip(apps) {
            *measure = (!app.is_null()).then(|| measure_of(app));
        }
        let other = &point["other"];
        resident.other = (!other.is_null()).then(|| ResidentOther {
            app_count: u32::try_from(other["app_count"].as_u64().unwrap()).unwrap(),
            resident: (!other["resident_bytes"].is_null())
                .then(|| measure_of(&other["resident_bytes"])),
        });

        resident
    }

    /// What the worker's `point` shows on the chart of `metric`.
    fn attribution_of(point: &Value, metric: AppAttributionMetric) -> Attribution {
        let mut attribution = Attribution {
            apps: [None; COHORT_MAX],
            other: None,
        };
        let apps = point["apps"].as_array().unwrap();
        for (reading, app) in attribution.apps.iter_mut().zip(apps) {
            *reading = reading_of(app, metric);
        }
        attribution.other = reading_of(&point["other"], metric).map(|reading| OtherReading {
            app_count: u32::try_from(point["other"]["app_count"].as_u64().unwrap()).unwrap(),
            reading,
        });

        attribution
    }

    /// The reading the worker's `value` shows on the chart of `metric`, `None` where it is null.
    fn reading_of(value: &Value, metric: AppAttributionMetric) -> Option<AttributionReading> {
        if value.is_null() {
            return None;
        }

        let measure = &value[metric.amount()];
        Some(AttributionReading {
            rate: value[metric.rate_name()].as_f64(),
            measure: (!measure.is_null()).then(|| measure_of(measure)),
            membership_complete: value["membership_complete"].as_bool().unwrap(),
        })
    }

    fn measure_of(value: &Value) -> Measure {
        let count = |name: &str| u32::try_from(value[name].as_u64().unwrap()).unwrap();

        Measure {
            known: value["known"].as_str().unwrap().parse().unwrap(),
            measured_members: count("measured_members"),
            unavailable_members: count("unavailable_members"),
            overflowed: value["overflowed"].as_bool().unwrap(),
        }
    }
}

/// Whether the newest second `view` holds is at whole wall second `wall_second`.
fn held_through(view: &View, wall_second: u64) -> bool {
    view.seconds
        .last()
        .is_some_and(|second| second.wall_ns / 1_000_000_000 == wall_second)
}
