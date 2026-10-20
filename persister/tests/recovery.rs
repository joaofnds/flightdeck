//! Recording recovery exercised through real ring, worker and disk-query boundaries.

#[cfg(test)]
#[path = "recovery_support/mod.rs"]
mod support;

#[cfg(test)]
mod tests {
    use super::support::{RecordingHarness, SourceFault};

    macro_rules! unavailable_source {
        ($name:ident, $fault:expr) => {
            #[test]
            fn $name() {
                // Goal: unavailable old sources cannot prevent a healthy new run sealing.
                // Method: retain a faulty source, restart, and query the new disk window.
                let mut recording = RecordingHarness::start();
                recording.leave_a_faulty_old_source($fault);

                recording.restart_collection_with_new_window();
                recording.restart_worker();
                recording.wait_for_new_window();

                assert_eq!(recording.processes_at_second(2_000), vec![(41, 101)]);
                recording.assert_old_source_retained();
                assert!(recording.new_window_prior_loss_unknown());
            }
        };
    }

    unavailable_source!(
        seals_healthy_history_after_a_corrupt_source,
        SourceFault::Corrupt
    );
    unavailable_source!(
        seals_healthy_history_after_a_refused_source,
        SourceFault::Permission
    );
    unavailable_source!(
        seals_healthy_history_after_a_foreign_source,
        SourceFault::Foreign
    );

    #[test]
    fn drains_a_tail_when_its_pid_is_now_owned_by_another_user() {
        // Goal: refused usage alone stays unknown, but a different real UID proves reuse.
        // Method: register root launchd's PID for our old tail and recover through the worker.
        let mut recording = RecordingHarness::start();

        recording.record_a_tail_with_a_pid_now_owned_by_another_user();
        recording.wait_for_worker_exit();

        assert_eq!(recording.processes_at_second(1_000), vec![(41, 7)]);
    }

    #[test]
    fn persists_new_retention_loss_as_unknown_extent_with_the_next_healthy_seal() {
        // Goal: an evicted unsealed tail remains explicit without invented missing counts.
        // Method: retain the worker across two rapid restarts and read its next prior seal.
        let mut recording = RecordingHarness::start();

        let losses = recording.evict_unsealed_history_with_surviving_worker();

        assert!(losses.unknown);
        assert_eq!(losses.publications, 0);
        assert_eq!(losses.events, 0);
    }

    #[test]
    fn keeps_reused_process_pids_distinct_by_their_measured_birth() {
        // Goal: a PID reused within one window never merges different process readings.
        // Method: publish the same PID with two births and read its typed disk identities.
        let mut recording = RecordingHarness::start();

        recording.record_reused_process_identity();
        recording.wait_for_worker_exit();

        assert_eq!(
            recording.old_process_identities(),
            vec![(41, 7, 11), (41, 8, 22)]
        );
    }

    #[test]
    fn seals_each_window_with_the_app_ranks_of_its_observations() {
        // Goal: the persister binary keeps each window's App ranks, so the viewer need not
        // replay the window to rank it.
        // Method: seal one window of a running process and list the tables it commits.
        let mut recording = RecordingHarness::start();

        recording.record_reused_process_identity();
        recording.wait_for_worker_exit();

        assert!(recording.old_window_tables(0).contains(&"app_ranks"));
    }

    #[test]
    fn seals_each_window_with_every_app_part_of_its_observations() {
        // Goal: the persister binary keeps each App's part of every observation of a window, so
        // the viewer need not replay the window for an App's history.
        // Method: seal one window of a running process and list the tables it commits.
        let mut recording = RecordingHarness::start();

        recording.record_reused_process_identity();
        recording.wait_for_worker_exit();

        assert!(recording.old_window_tables(0).contains(&"app_parts"));
    }

    #[test]
    fn records_the_exact_overwritten_publication_count_with_surviving_readings() {
        // Goal: overwritten history is explicit while recoverable observations stay exact.
        // Method: overfill the bounded ring before starting persistence and read the seal.
        let mut recording = RecordingHarness::start();

        recording.record_overwritten_history();
        recording.wait_for_worker_exit();

        recording.assert_exact_publication_loss(61);
        assert_eq!(recording.processes_at_second(1_061), vec![(41, 61)]);
        assert_eq!(recording.processes_at_second(1_060), vec![]);
    }

    #[test]
    fn blocks_new_seals_until_an_old_storage_failure_is_resolved() {
        // Goal: storage refusal cannot let new commits overtake recoverable old history.
        // Method: refuse storage, continue publishing, then restore it and recover in order.
        let mut recording = RecordingHarness::start();

        recording.refuse_storage_then_recover();

        assert_eq!(recording.processes_at_second(1_060), vec![(41, 60)]);
        assert_eq!(recording.processes_at_second(2_000), vec![(41, 101)]);
    }

    #[test]
    fn preserves_a_late_tick_and_sixty_scheduled_observations_in_one_elapsed_minute() {
        // Goal: the actual elapsed-minute key accommodates one carried delayed deadline.
        // Method: publish tick59 late beside scheduled ticks60..119 and drain the ring.
        let mut recording = RecordingHarness::start();

        recording.record_one_late_tick_before_a_full_scheduled_minute();
        recording.wait_for_worker_exit();

        assert_eq!(recording.old_window_tick_count(1), 61);
        assert_eq!(recording.processes_at_second(1_059), vec![(41, 59)]);
    }

    #[test]
    fn bounds_four_orderly_drains_without_counting_retention_loss() {
        // Goal: fully durable runs do not consume the two retained unsealed sources.
        // Method: drain four runs, inspect retained sources and query the original disk values.
        let mut recording = RecordingHarness::start();
        recording.finish_collection_and_drain(1);
        recording.restart_collection_with_new_window();
        recording.finish_collection_and_drain(2);
        recording.restart_collection_with_new_window();
        recording.finish_collection_and_drain(3);
        recording.restart_collection_with_new_window();
        recording.finish_collection_and_drain(4);

        recording.assert_two_completed_sources_retained_without_loss();
        assert_eq!(
            recording.processes_at_second(3_062),
            vec![(41, 1), (41, 2), (41, 3), (41, 4)]
        );
    }

    #[test]
    fn refuses_a_second_worker_while_the_surviving_owner_keeps_sealing() {
        // Goal: an ownership refusal identifies the survivor without stopping its work.
        // Method: launch another real worker while the owner spans two collector runs.
        let mut recording = RecordingHarness::start();
        recording.record_old_window_and_tail();
        recording.wait_for_old_full_window();

        recording.refuse_second_worker();
        recording.restart_collection_with_new_window();
        recording.wait_for_recovered_tail_and_new_window();

        recording.assert_second_worker_owned();
        assert_eq!(recording.processes_at_second(2_000), vec![(41, 101)]);
    }

    #[test]
    fn seals_healthy_history_after_an_unsupported_retained_source() {
        // Goal: unavailable retained history cannot prevent a healthy new run sealing.
        // Method: alter the old layout version, restart, and query only the new disk window.
        let mut recording = RecordingHarness::start();
        recording.leave_an_unsupported_old_source();

        recording.restart_collection_with_new_window();
        recording.restart_worker();
        recording.wait_for_new_window();

        assert_eq!(recording.processes_at_second(2_000), vec![(41, 101)]);
        recording.assert_old_source_retained();
        assert!(recording.new_window_prior_loss_unknown());
    }

    #[test]
    fn keeps_old_readings_once_before_new_seals_with_publication_number_continuity() {
        // Goal: a restart keeps the recoverable tail exactly once with ring number continuity.
        // Method: kill the worker after one seal, restart collection and query only disk.
        let mut recording = RecordingHarness::start();
        recording.record_old_window_and_tail();
        recording.wait_for_old_full_window();

        recording.kill_worker();
        recording.restart_collection_with_new_window();
        recording.restart_worker();
        recording.wait_for_recovered_tail_and_new_window();

        assert_eq!(
            recording.processes_at_second(1_059),
            vec![(41, 59), (42, 99)]
        );
        assert_eq!(recording.processes_at_second(1_060), vec![(41, 60)]);
        assert_eq!(recording.processes_at_second(2_000), vec![(41, 101)]);
        recording.assert_publication_number_continuity();
    }

    #[test]
    fn drains_a_dead_producers_tail_when_its_pid_names_a_newer_process() {
        // Goal: a reused PID cannot keep an earlier producer's partial tail unsealed.
        // Method: register a live PID with readings older than its birth and never finish.
        let mut recording = RecordingHarness::start();
        recording.record_a_tail_from_a_reused_producer_pid();

        recording.wait_for_worker_exit();

        assert_eq!(recording.processes_at_second(1_000), vec![(41, 7)]);
    }

    #[test]
    fn preserves_multiple_observations_in_a_backward_wall_clock_second() {
        // Goal: adjusting wall time cannot replace observations or change elapsed membership.
        // Method: publish repeated and backward seconds, seal and query from their wall times.
        let mut recording = RecordingHarness::start();

        recording.record_backward_wall_clock();
        recording.wait_for_old_full_window();

        assert_eq!(recording.processes_at_second(1_000), vec![(41, 0), (41, 1)]);
        assert_eq!(recording.processes_at_second(999), vec![(41, 2)]);
    }

    #[test]
    fn leaves_sleep_and_missed_windows_empty_without_inventing_zero_readings() {
        // Goal: elapsed gaps produce no empty seals or fabricated readings.
        // Method: jump from tick zero to tick 180, drain and query the intervening second.
        let mut recording = RecordingHarness::start();

        recording.record_elapsed_gap_and_finish();

        assert_eq!(recording.processes_at_second(1_000), vec![(41, 0)]);
        assert_eq!(recording.processes_at_second(1_180), vec![(41, 180)]);
        assert_eq!(recording.processes_at_second(1_100), vec![]);
        recording.assert_old_windows(&[0, 3]);
    }

    #[test]
    fn keeps_collecting_after_a_memory_guard_refuses_the_worker() {
        // Goal: a native-memory refusal cannot acknowledge a partial seal or stop sampling.
        // Method: force the worker's threshold below its baseline, reap it, then publish.
        let mut recording = RecordingHarness::start();

        recording.refuse_worker_with_low_memory_limit();

        recording.assert_worker_refused_without_a_commit();
        assert_eq!(recording.current_publication(), 3);
    }

    #[test]
    fn lets_the_surviving_owner_recover_a_new_collector_generation() {
        // Goal: surviving persistence serializes old recovery and a replacement collector.
        // Method: retain the worker while replacing its ring with a different capacity.
        let mut recording = RecordingHarness::start();
        recording.record_old_window_and_tail();
        recording.wait_for_old_full_window();

        recording.restart_collection_with_new_window();
        recording.continue_new_collection();
        recording.wait_for_recovered_tail_and_new_window();

        assert_eq!(recording.processes_at_second(1_060), vec![(41, 60)]);
        assert_eq!(recording.processes_at_second(2_000), vec![(41, 101)]);
    }

    #[test]
    fn recovers_registered_readings_after_the_discovery_lock_file_is_lost() {
        // Goal: durable registration preserves ring names independently of the discovery lock.
        // Method: remove only the lock file before restarting and recover its old tail.
        let mut recording = RecordingHarness::start();
        recording.record_old_window_and_tail();
        recording.wait_for_old_full_window();
        recording.kill_worker();

        recording.lose_discovery_lock();
        recording.restart_collection_with_new_window();
        recording.restart_worker();
        recording.wait_for_recovered_tail_and_new_window();

        assert_eq!(recording.processes_at_second(1_060), vec![(41, 60)]);
    }

    #[test]
    fn bounds_two_unsealed_restarts_and_reports_the_evicted_history() {
        // Goal: repeated restarts retain bounded source history and expose possible loss.
        // Method: restart twice with the worker stopped, then recover the two retained rings.
        let mut recording = RecordingHarness::start();
        recording.record_old_window_and_tail();
        recording.wait_for_old_full_window();
        recording.kill_worker();

        recording.restart_collection_with_new_window();
        recording.restart_collection_with_new_window();
        recording.restart_worker();
        recording.wait_for_new_window();

        assert_eq!(
            recording.processes_at_second(1_059),
            vec![(41, 59), (42, 99)]
        );
        assert_eq!(recording.processes_at_second(1_060), vec![]);
        recording.assert_one_retention_eviction();
    }

    #[test]
    fn reports_unknown_times_separately_without_hiding_known_matches() {
        // Goal: old readings lacking observation times stay accessible without guessed times.
        // Method: recover an untimed tail beside a timed window and query the known second.
        let mut recording = RecordingHarness::start();
        recording.record_old_window_and_tail();
        recording.wait_for_old_full_window();
        recording.kill_worker();

        recording.record_and_recover_untimed_tail();

        assert_eq!(
            recording.processes_at_second(1_059),
            vec![(41, 59), (42, 99)]
        );
        recording.assert_one_untimed_window_with_its_original_reading();
    }
}
