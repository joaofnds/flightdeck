//! Recording recovery exercised through real ring, worker and disk-query boundaries.

#[cfg(test)]
#[path = "recovery_support/mod.rs"]
mod support;

#[cfg(test)]
mod tests {
    use super::support::RecordingHarness;

    #[test]
    fn keeps_old_readings_once_before_new_seals_while_collection_continues() {
        // Goal: a restart keeps the recoverable tail exactly once while sampling continues.
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
        recording.assert_old_tail_committed_before_new_window();
        recording.assert_collection_advanced_during_recovery();
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
        recording.assert_old_tail_committed_before_new_window();
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
