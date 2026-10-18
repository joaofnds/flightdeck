import AppKit
import Combine
import Foundation
import SwiftUI

@main
struct RecordingStateChecks {
    @MainActor
    static func main() throws {
        if try runLiveCollectorChild(arguments: CommandLine.arguments) { return }
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--live-apps-paging-fixture" {
            try reads_live_apps_paging_through_the_real_worker(path: CommandLine.arguments[2])
            return
        }
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--live-apps-fixture" {
            try reads_live_apps_through_the_real_worker(path: CommandLine.arguments[2])
            return
        }
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--live-fixture" {
            try reads_the_latest_real_publication_after_rollback_and_repetition(
                path: CommandLine.arguments[2])
            try omits_unrequested_raw_pages_without_poisoning_later_queries(
                path: CommandLine.arguments[2])
            try reads_selected_fans_through_the_real_worker(path: CommandLine.arguments[2])
            try polls_catalog_without_reusing_or_decoding_a_metric_projection(
                path: CommandLine.arguments[2])
            return
        }
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--live-cpu-fixture" {
            try reads_exact_selected_cpu_activity_through_the_real_worker(path: CommandLine.arguments[2])
            try follows_cpu_history_through_the_real_worker(path: CommandLine.arguments[2])
            return
        }
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--live-cpu-delayed-fixture" {
            try recovers_selected_cpu_activity_after_a_delayed_commit(path: CommandLine.arguments[2])
            try retries_cpu_history_after_a_committed_source_recovers(path: CommandLine.arguments[2])
            return
        }
        precondition(CommandLine.arguments.count == 1, "Unknown native check arguments")
        runComputerInventoryChecks()
        try runSelectedCPUChecks()
        try runSelectedCPUPresentationChecks()
        try runCPUHistoryChecks()
        try runCPUHistoryPanelChecks()
        try runMemoryPressureChecks()
        try runMemorySwapChecks()
        try runMemoryQuantitiesChecks()
        try runFanHistoryChecks()
        try decodes_history_failure_without_rejecting_the_live_selection()
        try accepts_selection_from_a_source_without_history_diagnostics()
        try decodes_history_status_for_native_poll_cache_invalidation()
        try pages_cpu_history_without_stopping_the_live_window()
        try preserves_current_cpu_readings_when_history_is_unavailable()
        try clears_and_rejects_superseded_cpu_history()
        requests_cpu_history_only_for_the_actual_cpu_page()
        try installs_cpu_activity_when_entering_the_live_cpu_page()
        try clears_cpu_activity_before_a_new_selection_completes()
        try ignores_cpu_activity_from_a_superseded_completion()
        omits_cpu_activity_in_captured_reference_mode()
        completes_collector_startup_while_showing_computer_inventory()
        polls_without_projecting_metrics_while_showing_computer_inventory()
        keeps_discovering_the_collector_while_showing_computer_inventory()
        preserves_paused_metric_identity_through_computer_inventory()
        returns_from_computer_to_metric_destination("Memory", paused: false)
        returns_from_computer_to_metric_destination("GPU", paused: false)
        returns_from_computer_to_metric_destination("Energy", paused: false)
        returns_from_computer_to_metric_destination("Network", paused: false)
        returns_from_computer_to_metric_destination("Memory", paused: true)
        returns_from_computer_to_metric_destination("GPU", paused: true)
        returns_from_computer_to_metric_destination("Energy", paused: true)
        returns_from_computer_to_metric_destination("Network", paused: true)
        try preserves_selected_fan_channels_and_measured_zero()
        rejects_a_fan_page_with_a_missing_channel()
        try preserves_absent_and_invalid_fan_measurements_as_unavailable()
        try preserves_the_full_fan_bound_without_clamping_measured_rpm()
        rejects_invalid_selected_fan_page_structure()
        rejects_mismatched_selected_fan_observations()
        enters_overview_with_the_paused_exact_fan_observation()
        follows_fans_without_retaining_missing_measurements()
        clears_fans_before_loading_the_same_second()
        clears_fans_before_selecting_the_same_observation()
        clears_fans_before_reopening_the_same_recording()
        ignores_fans_from_a_superseded_completion()
        requests_fans_when_returning_from_captured_reference()
        retains_schematic_simulation_through_refresh()
        retains_schematic_simulation_through_selection()
        resets_schematic_simulation_for_source(path: "/other", live: true, run: "run", boot: "boot")
        resets_schematic_simulation_for_source(path: "/recording", live: false, run: "run", boot: "boot")
        resets_schematic_simulation_for_source(path: "/recording", live: true, run: "other", boot: "boot")
        resets_schematic_simulation_for_source(path: "/recording", live: true, run: "run", boot: "other")
        enters_processes_from_another_source()
        retains_refresh_after_a_later_selection()
        ignores_a_completed_stale_selection()
        preserves_process_and_observation_during_machine_paging()
        clears_old_readings_before_a_changed_second_completes()
        clears_the_previous_source_before_a_new_source_completes()
        try preserves_exact_recorded_cell_strings_and_unavailability()
        reports_native_query_errors()
        cancels_a_superseded_query_before_submitting_its_replacement()
        cancels_without_scheduling_another_read()
        surfaces_a_failed_query_cancellation()
        try distinguishes_publications_from_different_runs_and_boots()
        applies_native_table_selection_values_and_theme_changes()
        starts_live_without_building_a_poll_queue()
        requests_live_chart_without_a_raw_page("Overview")
        requests_live_chart_without_a_raw_page("Memory")
        requests_live_chart_without_a_raw_page("GPU")
        requests_live_chart_without_a_raw_page("Energy")
        requests_live_chart_without_a_raw_page("Network")
        retains_raw_pages_for_live_inspection_and_saved_sources()
        opens_raw_readings_at_the_paused_chart_observation_without_polling()
        starts_live_once_across_repeated_window_appearances()
        cancels_failed_startup_polling_with_reference_visible_and_resumes_on_retry()
        preserves_visible_readings_when_a_live_poll_is_unchanged()
        emits_no_changes_for_an_identical_background_poll()
        publishes_catalog_changes_without_replacing_the_selected_payload()
        publishes_clock_only_status_transitions_once()
        shows_foreground_loading_while_a_background_poll_is_being_canceled()
        advances_visible_readings_after_a_changed_background_poll()
        retains_poll_errors_until_success_without_republishing_identical_failures()
        keeps_live_tail_access_when_following_is_paused()
        keeps_paused_status_when_its_frozen_reading_ages()
        keeps_saved_recordings_on_the_existing_default_path()
        reports_a_stale_collector_using_the_injected_clock()
        reports_live_from_a_catalog_second_without_an_observation()
        freezes_a_pending_live_poll_when_following_is_paused()
        reports_a_future_tick_clock_without_trusting_the_catalog_maximum()
        reports_reader_losses_without_a_selected_tick()
        opening_a_recording_exits_captured_reference_mode()
        entering_memory_retains_the_selected_live_publication()
        entering_apps_retains_the_selected_live_publication()
        try installs_completed_live_app_metrics_and_history()
        try ignores_superseded_app_metrics_after_the_selected_key_changes()
        try adopts_effective_app_pages_for_the_next_live_poll()
        try explicit_observation_selection_resets_app_and_member_pages()
        entering_component_retains_the_selected_live_publication(.gpu)
        entering_component_retains_the_selected_live_publication(.energy)
        entering_component_retains_the_selected_live_publication(.network)
        entering_cpu_fetches_owned_tables_without_pausing_live()
        pins_visible_identity_when_pausing_an_active_cpu_read()
        pins_visible_identity_when_pausing_a_pending_cpu_read()
        cancels_latest_work_when_pausing_without_visible_readings()
        retains_exact_paused_identity_through_cpu_and_paging()
        chart_selection_requests_and_retains_exact_identity()
        unavailable_chart_identity_clears_readings_and_surfaces_error()
        print("Native recording state, selected fan and table checks passed")
        try runReferenceDataChecks()
        runDashboardChecks()
        try runOverviewInspectionChecks()
        try runRecordedMemoryChecks()
        try runRecordedComponentChecks()
        try runLiveCollectorChecks()
        try runLiveAppsChecks()
    }

    @MainActor
    private static func decodes_history_failure_without_rejecting_the_live_selection() throws {
        // Goal: historical unavailability is metadata rather than a failed live response.
        // Method: install a healthy native selection with a separate historical diagnostic.
        let selection = try historyFailureSelection()
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        let catalog = Catalog(path: "/recording", firstSecond: 17, lastSecond: 17, windows: 1,
            unknownClockTicks: 0, tables: [], live: true, latestObservation: selection.selected,
            historyError: selection.contextError)

        reader.finishNext(result: ReadResult(catalog: catalog, selection: selection, page: nil,
            machinePages: [:]))
        drainMainQueue()

        precondition(model.error == nil)
        precondition(model.following)
        precondition(model.selection?.selected?.publication == 9)
        precondition(model.selection?.summary?.cpuPercent == 25)
        precondition(model.selection?.trends.count == 1)
        precondition(model.selection?.contextError == "Earlier window unreadable")
    }

    private static func historyFailureSelection() throws -> Selection {
        let data = Data(#"{"value":{"second":17,"selected":{"run":"run-0","boot":"boot","publication":9,"run_tick":9,"wall_ns":17000000000},"alternatives":[{"run":"run-0","boot":"boot","publication":9,"run_tick":9,"wall_ns":17000000000}],"summary":{"run":"run-0","boot":"boot","publication":9,"wall_ns":17000000000,"segment":1,"cpu_percent":25},"trends":[{"run":"run-0","boot":"boot","publication":9,"wall_ns":17000000000,"segment":1,"cpu_percent":25}],"context_seconds":900,"context_error":"Earlier window unreadable"}}"#.utf8)

        return try decodeRecordingResponse(data)
    }

    private static func accepts_selection_from_a_source_without_history_diagnostics() throws {
        // Goal: sources without history diagnostics keep their existing selection contract.
        // Method: decode the preceding successful native shape without the optional field.
        let data = Data(#"{"value":{"second":17,"selected":null,"alternatives":[],"summary":null,"trends":[],"context_seconds":900}}"#.utf8)

        let selection: Selection = try decodeRecordingResponse(data)

        precondition(selection.second == 17)
        precondition(selection.contextError == nil)
    }

    private static func decodes_history_status_for_native_poll_cache_invalidation() throws {
        // Goal: poll caching observes historical status changes without requiring a new tick.
        // Method: decode the producer's catalog diagnostic through the native response adapter.
        let data = Data(#"{"value":{"path":"/recording","first_second":17,"last_second":17,"windows":1,"unknown_clock_ticks":0,"tables":[],"history_error":"Earlier window unreadable"}}"#.utf8)

        let catalog: Catalog = try decodeRecordingResponse(data)

        precondition(catalog.historyError == "Earlier window unreadable")
    }

    private static func preserves_selected_fan_channels_and_measured_zero() throws {
        // Goal: channel identity, measured zero and exact publication survive page decoding.
        // Method: reverse two fan rows at a publication beyond exact Double integers.
        let selected = Observation(run: "run", boot: "boot", publication: 9_007_199_254_740_993,
            runTick: 3, wallNs: 900_000_000_000, windowLosses: nil)
        let page = TablePage(name: "fans", columns: ["publication", "fan", "rpm"],
            rows: [["9007199254740993", "1", "0"], ["9007199254740993", "0", "1234.5"]],
            offset: 0, total: 2, observation: selected)

        let fans = try SelectedFanReadings(page: page, selected: selected)

        precondition(fans.observation == selected)
        precondition(fans.fan0 == 1234.5)
        precondition(fans.fan1 == 0)
    }

    private static func rejects_a_fan_page_with_a_missing_channel() {
        // Goal: an incomplete channel domain cannot masquerade as a complete fan page.
        // Method: provide channel 1 as the only row, despite source channels starting at 0.
        let selected = fanObservation()
        let page = fanPage([["3", "1", "1234"]], observation: selected)

        rejectsSelectedFanPage(page, selected: selected)
    }

    private static func preserves_absent_and_invalid_fan_measurements_as_unavailable() throws {
        // Goal: empty, absent and physically invalid readings never become measured zero.
        // Method: parse empty, one-channel, null/NaN and negative/infinite source pages.
        let selected = fanObservation()
        let empty = try SelectedFanReadings(page: fanPage([], observation: selected), selected: selected)
        let one = try SelectedFanReadings(page: fanPage([["3", "0", "1200"]],
            observation: selected), selected: selected)
        let absent = try SelectedFanReadings(page: fanPage([["3", "0", nil], ["3", "1", "NaN"]],
            observation: selected), selected: selected)
        let invalid = try SelectedFanReadings(page: fanPage([["3", "0", "-1"], ["3", "1", "inf"]],
            observation: selected), selected: selected)

        precondition(empty.fan0 == nil)
        precondition(empty.fan1 == nil)
        precondition(one.fan0 == 1200)
        precondition(one.fan1 == nil)
        precondition(absent.fan0 == nil)
        precondition(absent.fan1 == nil)
        precondition(invalid.fan0 == nil)
        precondition(invalid.fan1 == nil)
    }

    private static func preserves_the_full_fan_bound_without_clamping_measured_rpm() throws {
        // Goal: the source's full channel domain fits and QA slider limits do not alter readings.
        // Method: fill ten numbered channels with finite speeds above the slider range.
        let selected = fanObservation()
        let rows: [[String?]] = (0..<10).map { ["3", String($0), String(12_000 + $0 * 100)] }

        let fans = try SelectedFanReadings(page: fanPage(rows, observation: selected), selected: selected)

        precondition(fans.fan0 == 12_000)
        precondition(fans.fan1 == 12_100)
    }

    private static func rejects_invalid_selected_fan_page_structure() {
        // Goal: malformed, incomplete and oversized pages are refused before accepting channels.
        // Method: vary schema, offset, count, row width, publication, channel and numeric syntax.
        let selected = fanObservation()
        let columns = ["publication", "fan", "rpm"]
        let row: [String?] = ["3", "0", "1200"]

        rejectsSelectedFanPage(TablePage(name: "memory", columns: columns, rows: [row],
            offset: 0, total: 1, observation: selected), selected: selected)
        rejectsSelectedFanPage(TablePage(name: "fans", columns: columns, rows: [row],
            offset: 512, total: 1, observation: selected), selected: selected)
        rejectsSelectedFanPage(TablePage(name: "fans", columns: columns, rows: [row],
            offset: 0, total: 2, observation: selected), selected: selected)
        rejectsSelectedFanPage(TablePage(name: "fans", columns: columns,
            rows: Array(repeating: row, count: 11), offset: 0, total: 11,
            observation: selected), selected: selected)
        rejectsSelectedFanPage(TablePage(name: "fans", columns: ["publication", "fan", "fan"],
            rows: [row], offset: 0, total: 1, observation: selected), selected: selected)
        rejectsSelectedFanPage(fanPage([["3", "0"]], observation: selected), selected: selected)
        rejectsSelectedFanPage(fanPage([["2", "0", "1200"]], observation: selected), selected: selected)
        rejectsSelectedFanPage(fanPage([row, row], observation: selected), selected: selected)
        rejectsSelectedFanPage(fanPage([["3", "10", "1200"]], observation: selected), selected: selected)
        rejectsSelectedFanPage(fanPage([["3", nil, "1200"]], observation: selected), selected: selected)
        rejectsSelectedFanPage(fanPage([["3", "0", "rpm"]], observation: selected), selected: selected)
    }

    private static func rejects_mismatched_selected_fan_observations() {
        // Goal: a matching second cannot substitute a different observation or source.
        // Method: independently change each identity or clock field on the returned fan page.
        let selected = fanObservation()
        let rows: [[String?]] = [["3", "0", "1200"]]

        rejectsSelectedFanPage(fanPage(rows, observation: fanObservation(run: "other")),
            selected: selected)
        rejectsSelectedFanPage(fanPage(rows, observation: fanObservation(boot: "other")),
            selected: selected)
        rejectsSelectedFanPage(fanPage(rows, observation: fanObservation(publication: 4)),
            selected: selected)
        rejectsSelectedFanPage(fanPage(rows, observation: fanObservation(runTick: 4)),
            selected: selected)
        rejectsSelectedFanPage(fanPage(rows, observation: fanObservation(wallNs: 901_000_000_000)),
            selected: selected)
    }

    private static func fanObservation(run: String = "run", boot: String = "boot",
                                       publication: UInt64 = 3, runTick: UInt64? = nil,
                                       wallNs: UInt64 = 900_000_000_000) -> Observation {
        Observation(run: run, boot: boot, publication: publication, runTick: runTick ?? publication,
            wallNs: wallNs, windowLosses: nil)
    }

    @MainActor
    private static func enters_overview_with_the_paused_exact_fan_observation() {
        // Goal: entering Overview while paused reads its fans without following a newer tick.
        // Method: pause a completed Memory projection and inspect the next Overview request/result.
        let selected = fanObservation(run: "run-0", publication: 9)
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.route = "Memory"
        model.startLive()
        reader.finishNext(result: fanResult(selected: selected))
        drainMainQueue()
        model.setLive(false)

        model.enterOverview()

        precondition(reader.latestRequest?.includeFans == true)
        precondition(reader.latestRequest?.includeMachine == false)
        precondition(reader.latestRequest?.includePage == false)
        precondition(reader.latestRequest?.target == selected.identity)
        precondition(reader.latestRequest?.second == 900)
        precondition(!model.following)
        let reading = SelectedFanReadings(observation: selected, fan0: 1234, fan1: 0)
        reader.finishNext(result: fanResult(selected: selected, fans: reading))
        drainMainQueue()
        precondition(model.fans?.observation.identity == selected.identity)
        precondition(model.fans?.fan0 == 1234)
        precondition(model.fans?.fan1 == 0)
        precondition(!model.following)
    }

    private static func fanResult(selected: Observation, fans: SelectedFanReadings? = nil) -> ReadResult {
        let second = selected.wallNs / 1_000_000_000
        let selection = Selection(second: second, selected: selected, alternatives: [selected],
            summary: nil, trends: [], contextSeconds: 900)
        let catalog = Catalog(path: "/recording", firstSecond: second, lastSecond: second,
            windows: 0, unknownClockTicks: 0, tables: ["fans"], live: true,
            latestObservation: selected)

        return ReadResult(catalog: catalog, selection: selection, page: nil, machinePages: [:], fans: fans)
    }

    @MainActor
    private static func follows_fans_without_retaining_missing_measurements() {
        // Goal: unavailable current fans and a missing observation both clear older RPM readings.
        // Method: follow a valid point with an empty fan DTO, then a missing selected second.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        let selected = fanObservation(run: "run-0", publication: 9)
        model.startLive()
        reader.finishNext(result: fanResult(selected: selected,
            fans: SelectedFanReadings(observation: selected, fan0: 1234, fan1: 0)))
        drainMainQueue()

        model.pollLive()

        precondition(reader.latestRequest?.includeFans == true)
        precondition(reader.latestRequest?.second == nil)
        precondition(reader.latestRequest?.target == nil)
        let next = fanObservation(run: "run-0", publication: 10, wallNs: 901_000_000_000)
        reader.finishNext(result: fanResult(selected: next,
            fans: SelectedFanReadings(observation: next, fan0: nil, fan1: nil)))
        drainMainQueue()
        precondition(model.fans?.observation.identity == next.identity)
        precondition(model.fans?.fan0 == nil)
        precondition(model.fans?.fan1 == nil)
        model.pollLive()
        reader.finishNext(missingSecond: true)
        drainMainQueue()
        precondition(model.selection?.selected == nil)
        precondition(model.fans == nil)
    }

    @MainActor
    private static func clears_fans_before_loading_the_same_second() {
        // Goal: a new load cannot keep the previous fan payload while selection is absent.
        // Method: reload the same second and inspect state before its completion.
        let (model, reader, selected) = fanModel()

        model.load(second: selected.wallNs / 1_000_000_000)

        precondition(model.selection == nil)
        precondition(model.fans == nil)
        reader.finishNext(missingSecond: true)
        drainMainQueue()
    }

    @MainActor
    private static func clears_fans_before_selecting_the_same_observation() {
        // Goal: an explicit selection clears fans even if its identity equals the old one.
        // Method: select the visible exact target and inspect state before its completion.
        let (model, reader, selected) = fanModel()

        model.select(RecordedSelectionTarget(second: 900, identity: selected.identity))

        precondition(model.selection == nil)
        precondition(model.fans == nil)
        precondition(reader.latestRequest?.target == selected.identity)
        reader.finishNext(result: fanResult(selected: selected))
        drainMainQueue()
    }

    @MainActor
    private static func clears_fans_before_reopening_the_same_recording() {
        // Goal: reopening the same source path cannot retain a previous source's fan payload.
        // Method: choose the same path after a completed live fan read, before completing saved IO.
        let (model, reader, selected) = fanModel()
        precondition(model.fans?.observation.identity == selected.identity)

        model.chooseRecording(path: "/recording")

        precondition(model.selection == nil)
        precondition(model.fans == nil)
        precondition(reader.latestRequest?.live == false)
        reader.finishNext(missingSecond: true)
        drainMainQueue()
    }

    @MainActor
    private static func ignores_fans_from_a_superseded_completion() {
        // Goal: a superseded follow read cannot install its fan values over a pinned target.
        // Method: complete a canceled background read before the queued exact-identity read.
        let (model, reader, selected) = fanModel()
        model.pollLive()
        model.select(RecordedSelectionTarget(second: 900, identity: selected.identity))
        let superseded = fanObservation(run: "other", publication: 10)

        reader.finishNext(result: fanResult(selected: superseded,
            fans: SelectedFanReadings(observation: superseded, fan0: 8000, fan1: 7000)))
        drainMainQueue()

        precondition(model.selection == nil)
        precondition(model.fans == nil)
        precondition(reader.latestRequest?.target == selected.identity)
        reader.finishNext(result: fanResult(selected: selected,
            fans: SelectedFanReadings(observation: selected, fan0: 1234, fan1: 0)))
        drainMainQueue()
        precondition(model.selection?.selected?.identity == selected.identity)
        precondition(model.fans?.observation.identity == selected.identity)
        precondition(model.fans?.fan0 == 1234)
    }

    @MainActor
    private static func requests_fans_when_returning_from_captured_reference() {
        // Goal: captured reference mode never requests actual fans, and returning can fetch them.
        // Method: change reference visibility and explicitly enter actual Overview while paused.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false,
            showsReference: true)
        model.startLive()
        precondition(reader.latestRequest?.includeFans == false)
        reader.finishNext()
        drainMainQueue()
        model.setLive(false)

        model.showsReference = false
        model.enterOverview()

        precondition(reader.latestRequest?.includeFans == true)
        precondition(!model.following)
        reader.finishNext()
        drainMainQueue()
    }

    @MainActor
    private static func retains_schematic_simulation_through_refresh() {
        // Goal: a same-source pending read and its completion keep explicit QA simulation.
        // Method: refresh the real model through its controlled reader and forward its UI source.
        let (model, reader, selected) = fanModel()
        let preview = SchematicPreviewState()
        preview.observeSource(model.schematicPreviewSource)
        preview.overrides[.cpuTemperature] = 90

        model.load(refresh: true)
        preview.observeSource(model.schematicPreviewSource)

        precondition(model.selection == nil)
        precondition(preview.overrides[.cpuTemperature] == 90)
        reader.finishNext(result: fanResult(selected: selected))
        drainMainQueue()
        preview.observeSource(model.schematicPreviewSource)
        precondition(preview.overrides[.cpuTemperature] == 90)
        model.chooseRecording(path: "/other")
        preview.observeSource(model.schematicPreviewSource)
        precondition(preview.overrides.isEmpty)
        reader.finishNext(missingSecond: true)
        drainMainQueue()
    }

    @MainActor
    private static func retains_schematic_simulation_through_selection() {
        // Goal: selecting within a run preserves simulation through absence and later ticks.
        // Method: commit the selected identity, complete its read, then install a following tick.
        let (model, reader, selected) = fanModel()
        let preview = SchematicPreviewState()
        preview.observeSource(model.schematicPreviewSource)
        preview.overrides[.fan0] = 5000

        model.select(RecordedSelectionTarget(second: 900, identity: selected.identity))
        preview.observeSource(model.schematicPreviewSource)

        precondition(model.selection == nil)
        precondition(preview.overrides[.fan0] == 5000)
        reader.finishNext(result: fanResult(selected: selected))
        drainMainQueue()
        preview.observeSource(model.schematicPreviewSource)
        precondition(preview.overrides[.fan0] == 5000)
        model.setLive(true)
        reader.finishNext(result: fanResult(selected: fanObservation(run: selected.run, publication: 10)))
        drainMainQueue()
        preview.observeSource(model.schematicPreviewSource)
        precondition(preview.overrides[.fan0] == 5000)
        model.chooseRecording(path: "/recording")
        preview.observeSource(model.schematicPreviewSource)
        precondition(preview.overrides.isEmpty)
        reader.finishNext(missingSecond: true)
        drainMainQueue()
    }

    @MainActor
    private static func resets_schematic_simulation_for_source(path: String, live: Bool,
                                                               run: String, boot: String) {
        // Goal: temporary absence does not hide a later genuine source identity change.
        // Method: observe a known source, an absent selection, then a distinct source dimension.
        let preview = SchematicPreviewState()
        preview.observeSource(SchematicPreviewSource(path: "/recording", live: true,
            run: "run", boot: "boot"))
        preview.overrides[.gpuTemperature] = 45

        preview.observeSource(SchematicPreviewSource(path: "/recording", live: true,
            run: nil, boot: nil))

        precondition(preview.overrides[.gpuTemperature] == 45)
        preview.observeSource(SchematicPreviewSource(path: path, live: live, run: run, boot: boot))
        precondition(preview.overrides.isEmpty)
    }

    @MainActor
    private static func fanModel() -> (RecordingModel, ControlledRecordingReader, Observation) {
        let selected = fanObservation(run: "run-0", publication: 9)
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext(result: fanResult(selected: selected,
            fans: SelectedFanReadings(observation: selected, fan0: 1234, fan1: 0)))
        drainMainQueue()
        precondition(model.fans?.fan0 == 1234)

        return (model, reader, selected)
    }

    @MainActor
    private static func reads_selected_fans_through_the_real_worker(path: String) throws {
        // Goal: selected-only fan reads follow exact identities and respect projection caching.
        // Method: change projection flags, pin a repeated-second sibling, then follow again.
        let reader = RecordingWorker()
        let chart = try nativeRead(reader, request: fanWorkerRequest(path: path, includeFans: false))
        precondition(chart.fans == nil)

        let current = try nativeRead(reader, request: fanWorkerRequest(path: path, includeFans: true))

        precondition(!current.unchanged)
        precondition(current.page == nil)
        precondition(current.machinePages.isEmpty)
        precondition(current.fans?.observation.identity == current.selection?.selected?.identity)
        precondition(current.fans?.observation.publication == 3)
        precondition(current.fans?.fan0 == 1234.5)
        precondition(current.fans?.fan1 == 0)
        let unchanged = try nativeRead(reader, request: fanWorkerRequest(path: path, includeFans: true))
        precondition(unchanged.unchanged)
        precondition(unchanged.fans?.fan0 == 1234.5)
        let omitted = try nativeRead(reader, request: fanWorkerRequest(path: path, includeFans: false))
        precondition(!omitted.unchanged)
        precondition(omitted.fans == nil)
        guard let selected = current.selection?.selected else { preconditionFailure() }
        let sibling = RecordedObservationIdentity(run: selected.run, boot: selected.boot, publication: 2)
        let pinned = try nativeRead(reader,
            request: fanWorkerRequest(path: path, includeFans: true, target: sibling))
        precondition(pinned.selection?.selected?.identity == sibling)
        precondition(pinned.fans?.observation.identity == sibling)
        precondition(pinned.fans?.fan0 == nil)
        precondition(pinned.fans?.fan1 == nil)
        let latest = try nativeRead(reader, request: fanWorkerRequest(path: path, includeFans: true))
        precondition(latest.selection?.selected?.identity == selected.identity)
        precondition(latest.fans?.observation.identity == selected.identity)
        precondition(latest.fans?.fan0 == 1234.5)
        print("Real RecordingWorker keeps selected fans bounded, optional and exact across projections")
    }

    @MainActor
    private static func reads_exact_selected_cpu_activity_through_the_real_worker(path: String) throws {
        // Goal: native CPU activity uses exact live identities and an independent optional projection.
        // Method: read the genuine three-publication CPU fixture, pin its middle tick, then follow.
        let reader = RecordingWorker()
        let omitted = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: false))
        precondition(omitted.cpuDetail == nil)

        let current = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true))

        precondition(!current.unchanged)
        precondition(current.page == nil)
        precondition(current.machinePages.isEmpty)
        precondition(current.cpuDetail?.observation == current.selection?.selected)
        precondition(current.cpuDetail?.observation.publication == 3)
        precondition(current.cpuDetail?.cpus == [LogicalCPUReading(cpu: 0, busyPercent: 25),
            LogicalCPUReading(cpu: 1, busyPercent: 75)])
        let cached = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true))
        precondition(cached.unchanged)
        precondition(cached.cpuDetail?.cpus == current.cpuDetail?.cpus)
        guard let selected = current.selection?.selected else { preconditionFailure() }
        let earlier = RecordedObservationIdentity(run: selected.run, boot: selected.boot, publication: 2)
        let pinned = try nativeRead(reader,
            request: cpuWorkerRequest(path: path, include: true, target: earlier))
        precondition(pinned.selection?.selected?.identity == earlier)
        precondition(pinned.cpuDetail?.observation.identity == earlier)
        precondition(pinned.cpuDetail?.cpus == [LogicalCPUReading(cpu: 0, busyPercent: 0),
            LogicalCPUReading(cpu: 1, busyPercent: 100)])
        let latest = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true))
        precondition(latest.cpuDetail?.observation == selected)
        precondition(latest.cpuDetail?.cpus == current.cpuDetail?.cpus)
        let omittedAgain = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: false))
        precondition(!omittedAgain.unchanged)
        precondition(omittedAgain.cpuDetail == nil)
        print("Real RecordingWorker preserves optional and exact selected logical CPU activity")
    }

    @MainActor
    private static func follows_cpu_history_through_the_real_worker(path: String) throws {
        // Goal: native CPU history follows the same exact common context without forcing replay.
        // Method: read the real live fixture, reuse its unchanged page, pin, then follow again.
        let reader = RecordingWorker()
        let current = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            historyOffset: 0))
        guard let history = current.cpuHistory?.readings else {
            preconditionFailure("Live CPU history was not projected")
        }

        precondition(history.observation == current.selection?.selected)
        precondition(history.observations.map(\.observation.identity)
            == current.selection?.trends.map(\.identity))
        precondition(history.cpus[0].busyPercent == [nil, 0, 25])
        precondition(history.cpus[1].busyPercent == [nil, 100, 75])
        let cached = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            historyOffset: 0))
        precondition(cached.unchanged)
        precondition(cached.cpuHistory?.readings?.cpus[0].busyPercent == [nil, 0, 25])
        let target = RecordedObservationIdentity(run: history.observation.run,
            boot: history.observation.boot, publication: 2)
        let pinned = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            target: target, historyOffset: 0))
        precondition(pinned.cpuHistory?.readings?.observation.identity == target)
        precondition(pinned.cpuHistory?.readings?.cpus[0].busyPercent == [nil, 0])
        let latest = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            historyOffset: 0))
        precondition(latest.cpuHistory?.readings?.observation == history.observation)
        let omitted = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true))
        precondition(!omitted.unchanged)
        precondition(omitted.cpuHistory == nil)
        let normalized = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            historyOffset: 32))
        precondition(normalized.cpuHistory?.readings?.offset == 0)
        let invalid = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            historyOffset: 1))
        precondition(invalid.cpuHistory?.notice != nil)
        precondition(invalid.selection?.summary?.cpuPercent == 50)
        precondition(invalid.cpuDetail?.cpus.first?.busyPercent == 25)
        let retry = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            historyOffset: 1))
        precondition(!retry.unchanged, "A transient history failure was cached indefinitely")
        let recovered = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true,
            historyOffset: 0))
        precondition(recovered.cpuHistory?.notice == nil)
        precondition(recovered.cpuHistory?.readings?.observation == history.observation)
        print("Real worker follows exact independent CPU history and preserves current readings on failure")
    }

    private static func cpuWorkerRequest(path: String, include: Bool,
                                        target: RecordedObservationIdentity? = nil,
                                        historyOffset: UInt32? = nil) -> ReadRequest {
        ReadRequest(path: path, second: target == nil ? nil : 2002, alternative: 0, table: 1,
            offset: 0, refresh: false, machineOffsets: [:], includeMachine: false,
            includePage: false, includeFans: false, live: true, poll: true, target: target,
            includeCPUDetail: include, cpuHistoryOffset: historyOffset)
    }

    @MainActor
    private static func recovers_selected_cpu_activity_after_a_delayed_commit(path: String) throws {
        // Goal: a newly discovered predecessor repairs CPU bars without a new publication.
        // Method: select the oldest retained tick, then seal its expired predecessor on request.
        let reader = RecordingWorker()
        let current = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: false))
        guard let latest = current.selection?.selected else { preconditionFailure() }
        let target = RecordedObservationIdentity(run: latest.run, boot: latest.boot, publication: 3)
        let request = ReadRequest(path: path, second: 2003, alternative: 0, table: 1,
            offset: 0, refresh: false, machineOffsets: [:], includeMachine: false,
            includePage: false, includeFans: false, live: true, poll: true, target: target,
            includeCPUDetail: true)
        let unavailable = try nativeRead(reader, request: request)
        precondition(unavailable.selection?.selected?.identity == target)
        precondition(unavailable.cpuDetail?.intervalAvailable == false)
        precondition(unavailable.cpuDetail?.previousObservation == nil)
        let root = URL(fileURLWithPath: path, isDirectory: true)
        try Data().write(to: root.appendingPathComponent("cpu-commit-request"))
        let deadline = Date().addingTimeInterval(5)
        while !FileManager.default.fileExists(atPath: root.appendingPathComponent("cpu-commit-ready").path) {
            precondition(Date() < deadline, "Fixture did not seal the delayed CPU predecessor")
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }

        let recovered = try nativeRead(reader, request: request)

        precondition(!recovered.unchanged, "Delayed CPU commit reused the unavailable projection")
        precondition(recovered.selection?.selected?.identity == target)
        precondition(recovered.cpuDetail?.intervalAvailable == true)
        precondition(recovered.cpuDetail?.cpus == [LogicalCPUReading(cpu: 0, busyPercent: 25),
            LogicalCPUReading(cpu: 1, busyPercent: 75)])
        let repeated = try nativeRead(reader, request: request)
        precondition(repeated.unchanged)
        precondition(repeated.cpuDetail?.cpus == recovered.cpuDetail?.cpus)
        print("Real RecordingWorker repairs selected CPU activity after a delayed predecessor commit")
    }

    @MainActor
    private static func retries_cpu_history_after_a_committed_source_recovers(path: String) throws {
        // Goal: a CPU-only query error stays local and retries without another publication.
        // Method: warm scalar context, hide committed data, repeat the failure, then restore it.
        let reader = RecordingWorker()
        let warm = try nativeRead(reader, request: cpuWorkerRequest(path: path, include: true))
        let request = cpuWorkerRequest(path: path, include: true, historyOffset: 0)
        let root = URL(fileURLWithPath: path, isDirectory: true)
        let data = root.appendingPathComponent("data", isDirectory: true)
        let held = root.appendingPathComponent("held-cpu-history", isDirectory: true)
        try FileManager.default.moveItem(at: data, to: held)
        defer {
            if FileManager.default.fileExists(atPath: held.path) {
                do { try FileManager.default.moveItem(at: held, to: data) }
                catch { preconditionFailure("Could not restore fixture data: \(error)") }
            }
        }

        for _ in 0..<3 {
            let failed = try nativeRead(reader, request: request)
            precondition(!failed.unchanged, "CPU history failure suppressed its next retry")
            precondition(failed.cpuHistory?.notice != nil, "CPU query failure lost its local notice")
            precondition(failed.cpuHistory?.readings?.observation == warm.selection?.selected)
            precondition(failed.cpuHistory?.readings?.cpus[0].busyPercent.last
                == warm.cpuDetail?.cpus[0].busyPercent)
            precondition(failed.catalog.historyError == warm.catalog.historyError)
            precondition(failed.selection?.selected == warm.selection?.selected)
            precondition(failed.selection?.summary?.cpuPercent == warm.selection?.summary?.cpuPercent)
            precondition(failed.cpuDetail?.cpus == warm.cpuDetail?.cpus)
        }
        try FileManager.default.moveItem(at: held, to: data)
        let recovered = try nativeRead(reader, request: request)
        precondition(!recovered.unchanged)
        precondition(recovered.cpuHistory?.notice == nil)
        precondition(recovered.cpuHistory?.readings?.observation == warm.selection?.selected)
        let cached = try nativeRead(reader, request: request)
        precondition(cached.unchanged)
        print("Real worker retries CPU history source failures without moving live selection")
    }

    @MainActor
    private static func pages_cpu_history_without_stopping_the_live_window() throws {
        // Goal: paging CPUs keeps the same visible observation while live following continues.
        // Method: complete two history pages, poll, then adopt a smaller returned CPU domain.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.route = "CPU & processes"
        model.startLive()
        reader.finishNext(result: try cpuHistoryModelResult(publication: 10))
        drainMainQueue()
        let selected = model.selection?.selected

        model.pageCPUHistory(32)

        precondition(model.following)
        precondition(model.selection?.selected == selected)
        precondition(reader.latestRequest?.cpuHistoryOffset == 32)
        precondition(reader.latestRequest?.second == nil)
        precondition(reader.latestRequest?.target == nil)
        reader.finishNext(result: try cpuHistoryModelResult(publication: 10, offset: 32))
        drainMainQueue()
        precondition(model.cpuHistory?.readings?.offset == 32)
        precondition(model.cpuHistory?.readings?.cpus.first?.cpu == 32)
        model.pollLive()
        precondition(reader.latestRequest?.cpuHistoryOffset == 32)
        reader.finishNext(result: try cpuHistoryModelResult(publication: 11, total: 2))
        drainMainQueue()
        model.pollLive()
        precondition(reader.latestRequest?.cpuHistoryOffset == 0)
        reader.finishNext(result: try cpuHistoryModelResult(publication: 12, total: 2))
        drainMainQueue()
        precondition(model.following)
        precondition(model.selection?.selected?.publication == 12)
    }

    @MainActor
    private static func preserves_current_cpu_readings_when_history_is_unavailable() throws {
        // Goal: an unavailable history panel cannot erase healthy current CPU readings.
        // Method: follow a typed history failure with a successful recovered history page.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.route = "CPU & processes"
        model.startLive()

        reader.finishNext(result: try cpuHistoryModelResult(publication: 10,
            notice: "CPU history unavailable"))
        drainMainQueue()

        precondition(model.error == nil)
        precondition(model.selection?.summary?.cpuPercent == 50)
        precondition(model.cpuDetail?.cpus.first?.busyPercent == 0)
        precondition(model.cpuHistory?.readings == nil)
        precondition(model.cpuHistory?.notice == "CPU history unavailable")
        precondition(model.following)
        model.pollLive()
        reader.finishNext(result: try cpuHistoryModelResult(publication: 11))
        drainMainQueue()
        precondition(model.cpuHistory?.readings?.observation.publication == 11)
        precondition(model.cpuHistory?.notice == nil)
    }

    @MainActor
    private static func clears_and_rejects_superseded_cpu_history() throws {
        // Goal: an old follow completion cannot supply history to a newer exact selection.
        // Method: supersede a poll, finish it, then complete the pinned replacement.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.route = "CPU & processes"
        model.startLive()
        reader.finishNext(result: try cpuHistoryModelResult(publication: 10))
        drainMainQueue()
        model.pollLive()
        let next = try cpuHistoryModelResult(publication: 12)
        guard let selected = next.selection?.selected else { preconditionFailure() }

        model.select(RecordedSelectionTarget(second: 12, identity: selected.identity))

        precondition(model.cpuHistory == nil)
        reader.finishNext(result: try cpuHistoryModelResult(publication: 11))
        drainMainQueue()
        precondition(model.cpuHistory == nil)
        precondition(reader.latestRequest?.target == selected.identity)
        precondition(reader.latestRequest?.cpuHistoryOffset == 0)
        reader.finishNext(result: next)
        drainMainQueue()
        precondition(model.cpuHistory?.readings?.observation == selected)
        precondition(!model.following)
        model.chooseRecording(path: "/other")
        precondition(model.cpuHistory == nil)
        reader.finishNext(result: try cpuHistoryModelResult(publication: 13))
        drainMainQueue()
    }

    private static func cpuHistoryModelResult(publication: UInt64, offset: UInt32 = 0,
        total: UInt32 = 64, notice: String? = nil) throws -> ReadResult {
        let base = try cpuModelResult(publication: publication)
        guard let selected = base.selection?.selected else { preconditionFailure() }
        let previous = Observation(run: selected.run, boot: selected.boot,
            publication: publication - 1, runTick: publication - 1,
            wallNs: selected.wallNs - 1_000_000_000, windowLosses: nil)
        let metricData = try JSONSerialization.data(withJSONObject: ["value": [
            "run": selected.run, "boot": selected.boot, "publication": publication,
            "run_tick": publication, "wall_ns": selected.wallNs, "segment": 0, "cpu_percent": 50]])
        let metric: Metric = try decodeRecordingResponse(metricData)
        let selection = Selection(second: publication, selected: selected, alternatives: [selected],
            summary: metric, trends: [metric], contextSeconds: 900)
        let rows = (offset..<min(offset + 32, total)).map { cpu in
            LogicalCPUHistoryRow(cpu: cpu, busyPercent: [cpu == 0 ? 0 : 50])
        }
        let payload = CPUHistoryPayload(observation: selected, contextSeconds: 900, firstNs: 0,
            endNs: selected.wallNs + 1_000_000_000, offset: offset, total: total,
            retentionLimited: false, observations: [CPUHistoryObservation(observation: selected,
                previousObservation: previous, intervalAvailable: true, cpuCount: total)], cpus: rows)
        let history = try SelectedCPUHistory(payload: payload, selection: selection)
        let current = try SelectedCPUReadings(payload: CPUDetailPayload(observation: selected,
            previousObservation: previous, intervalAvailable: true, unavailableReason: nil,
            cpus: (0..<total).map { LogicalCPUReading(cpu: $0, busyPercent: $0 == 0 ? 0 : 50) }),
            selected: selected)

        return ReadResult(catalog: base.catalog, selection: selection, page: nil,
            machinePages: [:], cpuDetail: current,
            cpuHistory: notice.map { CPUHistoryRead.unavailable($0) } ?? .readings(history))
    }

    @MainActor
    private static func requests_cpu_history_only_for_the_actual_cpu_page() {
        // Goal: only the visible actual CPU pane requests its bounded history projection.
        // Method: enter from live Overview, then poll another route and reference CPU.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        precondition(reader.latestRequest?.cpuHistoryOffset == nil)
        reader.finishNext()
        drainMainQueue()

        model.enterComponent(.cpu)

        precondition(reader.latestRequest?.cpuHistoryOffset == 0)
        precondition(model.following)
        reader.finishNext()
        drainMainQueue()
        model.navigate(to: "GPU")
        model.pollLive()
        precondition(reader.latestRequest?.cpuHistoryOffset == nil)
        reader.finishNext()
        drainMainQueue()
        model.showsReference = true
        model.enterComponent(.cpu)
        precondition(reader.latestRequest?.cpuHistoryOffset == nil)
        reader.finishNext()
        drainMainQueue()
    }

    @MainActor
    private static func installs_cpu_activity_when_entering_the_live_cpu_page() throws {
        // Goal: CPU entry requests and installs its independent detail without changing live mode.
        // Method: enter from Overview using the controlled I/O boundary and complete a real DTO.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        precondition(reader.latestRequest?.includeCPUDetail == false)
        reader.finishNext()
        drainMainQueue()
        let result = try cpuModelResult(publication: 10)

        model.enterComponent(.cpu)
        reader.finishNext(result: result)
        drainMainQueue()

        precondition(reader.latestRequest?.includeCPUDetail == true)
        precondition(model.following)
        precondition(model.cpuDetail?.observation == model.selection?.selected)
        precondition(model.cpuDetail?.cpus == result.cpuDetail?.cpus)
    }

    @MainActor
    private static func clears_cpu_activity_before_a_new_selection_completes() throws {
        // Goal: a pending exact selection never displays bars from a previous observation.
        // Method: install CPU data, select the same observation, then complete with no detail.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        let result = try cpuModelResult(publication: 10)
        model.enterComponent(.cpu)
        reader.finishNext(result: result)
        drainMainQueue()
        precondition(model.cpuDetail != nil)
        guard let selected = result.selection?.selected else { preconditionFailure() }

        model.select(RecordedSelectionTarget(second: 10, identity: selected.identity))

        precondition(model.cpuDetail == nil)
        reader.finishNext(result: fanResult(selected: selected))
        drainMainQueue()
        precondition(model.cpuDetail == nil)
    }

    @MainActor
    private static func ignores_cpu_activity_from_a_superseded_completion() throws {
        // Goal: a late live result cannot replace the CPU activity of a newer exact selection.
        // Method: supersede an in-flight read and finish it before the replacement.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        let old = try cpuModelResult(publication: 10)
        let next = try cpuModelResult(publication: 11)
        model.enterComponent(.cpu)
        guard let selected = next.selection?.selected else { preconditionFailure() }

        model.select(RecordedSelectionTarget(second: 11, identity: selected.identity))
        reader.finishNext(result: old)
        drainMainQueue()

        precondition(model.cpuDetail == nil)
        precondition(reader.latestRequest?.target == selected.identity)
        reader.finishNext(result: next)
        drainMainQueue()
        precondition(model.cpuDetail?.observation == selected)
        precondition(model.cpuDetail?.cpus == next.cpuDetail?.cpus)
    }

    @MainActor
    private static func omits_cpu_activity_in_captured_reference_mode() {
        // Goal: actual CPU detail is not queried while showing captured reference visuals.
        // Method: enter CPU with the existing captured-reference model option.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false,
            showsReference: true)

        model.enterComponent(.cpu)

        precondition(reader.latestRequest?.includeCPUDetail == false)
        reader.finishNext()
        drainMainQueue()
    }

    private static func cpuModelResult(publication: UInt64) throws -> ReadResult {
        let selected = Observation(run: "run-0", boot: "boot", publication: publication,
            runTick: publication, wallNs: publication * 1_000_000_000, windowLosses: nil)
        let previous = Observation(run: selected.run, boot: selected.boot, publication: publication - 1,
            runTick: publication - 1, wallNs: (publication - 1) * 1_000_000_000, windowLosses: nil)
        let payload = CPUDetailPayload(observation: selected, previousObservation: previous,
            intervalAvailable: true, unavailableReason: nil,
            cpus: [LogicalCPUReading(cpu: 0, busyPercent: 25), LogicalCPUReading(cpu: 1, busyPercent: 75)])
        let readings = try SelectedCPUReadings(payload: payload, selected: selected)
        let result = fanResult(selected: selected)

        return ReadResult(catalog: result.catalog, selection: result.selection, page: nil,
            machinePages: [:], cpuDetail: readings)
    }

    private static func fanWorkerRequest(path: String, includeFans: Bool,
                                          target: RecordedObservationIdentity? = nil) -> ReadRequest {
        ReadRequest(path: path, second: target == nil ? nil : 900, alternative: 0, table: 1,
            offset: 0, refresh: false, machineOffsets: [:], includeMachine: false,
            includePage: false, includeFans: includeFans, live: true, poll: true, target: target)
    }

    private static func fanPage(_ rows: [[String?]], observation: Observation) -> TablePage {
        TablePage(name: "fans", columns: ["publication", "fan", "rpm"], rows: rows,
            offset: 0, total: UInt64(rows.count), observation: observation)
    }

    private static func rejectsSelectedFanPage(_ page: TablePage, selected: Observation) {
        do {
            let accepted = try SelectedFanReadings(page: page, selected: selected)
            preconditionFailure("Accepted invalid fan page for \(accepted.observation.publication)")
        } catch {
            precondition(error is RecordingError)
        }
    }

    @MainActor
    private static func installs_completed_live_app_metrics_and_history() throws {
        // Goal: the native App pane receives the completed metrics and exact selected observation.
        // Method: finish the existing controlled reader with the independent App DTO fixture.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext()
        drainMainQueue()
        let result = try appModelResult(LiveAppsFixture.read())

        model.enterApps()
        reader.finishNext(result: result)
        drainMainQueue()
        let installed = try appModelPayload(model.apps)

        precondition(model.appKey == LiveAppKeyIdentity(kind: .bundle, bytes: Data("/A.app".utf8)),
            "Completed Apps read did not install its exact key")
        precondition(installed.key == model.appKey)
        precondition(model.selection?.selected?.identity == result.selection?.selected?.identity)
        precondition(model.apps?.list.header.selected == result.apps?.list.header.selected)
        precondition(model.apps?.detail?.app.cpuNs.known.value == 500_000_000)
        precondition(model.apps?.detail?.processes.map(\.pid) == [42, 43, 44])
        precondition(model.apps?.history?.points.first?.cpuPercentOneCore == 50)
        precondition(model.apps?.history?.selected == result.apps?.list.header.selected)
        precondition(model.apps?.history?.points.first?.id
            == "00000000000000030000000000000004:00000000000000050000000000000006:9007199254740993")
    }

    @MainActor
    private static func ignores_superseded_app_metrics_after_the_selected_key_changes() throws {
        // Goal: a late App response cannot overwrite a more recent selected App's facts.
        // Method: change A to B while A is in flight, then complete both real model callbacks.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext()
        drainMainQueue()
        let old = try appModelResult(LiveAppsFixture.read())
        let next = try appModelResult(LiveAppsFixture.read(otherApp: true))
        let nextKey = LiveAppKeyIdentity(kind: .bundle, bytes: Data("/B.app".utf8))
        model.enterApps()
        reader.finishNext(result: old)
        drainMainQueue()
        model.selectApp(LiveAppKeyIdentity(kind: .bundle, bytes: Data("/A.app".utf8)))

        model.selectApp(nextKey)
        reader.finishNext(result: old)
        drainMainQueue()

        precondition(model.appKey == nextKey, "Superseded App result replaced the selected key")
        precondition(model.apps == nil, "Superseded App result became visible")
        precondition(reader.latestRequest?.apps?.key == nextKey)

        reader.finishNext(result: next)
        drainMainQueue()

        precondition(model.appKey == nextKey)
        precondition(model.apps?.detail?.app.key.identity == nextKey)
        precondition(model.apps?.history?.selected == next.apps?.list.header.selected)
        precondition(model.selection?.selected?.identity == next.selection?.selected?.identity)
    }

    @MainActor
    private static func adopts_effective_app_pages_for_the_next_live_poll() throws {
        // Goal: the next poll uses the worker's resolved pages after either population shrinks.
        // Method: complete a 512/512 request with coherent zero-page App payloads.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        let result = try appModelResult(LiveAppsFixture.read())
        model.startLive()
        reader.finishNext()
        drainMainQueue()
        model.enterApps()
        reader.finishNext(result: result)
        drainMainQueue()
        model.pageApps(512)
        model.pageAppMembers(512)
        reader.finishNext(result: result)
        drainMainQueue()
        precondition(reader.latestRequest?.apps?.appsOffset == 512)
        precondition(reader.latestRequest?.apps?.membersOffset == 512)
        reader.finishNext(result: result)
        drainMainQueue()

        model.pollLive()

        precondition(reader.latestRequest?.apps?.appsOffset == 0,
            "The next live poll retained an invalid App page")
        precondition(reader.latestRequest?.apps?.membersOffset == 0,
            "The next live poll retained an invalid process page")
        precondition(reader.latestRequest?.apps?.key == result.apps?.key)
        precondition(model.selection?.selected?.identity == result.selection?.selected?.identity)
        precondition(model.apps?.detail?.header.selected == result.apps?.list.header.selected)
    }

    @MainActor
    private static func explicit_observation_selection_resets_app_and_member_pages() throws {
        // Goal: explicit observation navigation starts both App tables from their first pages.
        // Method: select an exact observation while a 512/512 App request is pending.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        let result = try appModelResult(LiveAppsFixture.read())
        let target = RecordedSelectionTarget(second: 1_800_000_000,
            identity: result.selection!.selected!.identity)
        model.startLive()
        reader.finishNext()
        drainMainQueue()
        model.enterApps()
        reader.finishNext(result: result)
        drainMainQueue()
        model.pageApps(512)
        model.pageAppMembers(512)
        reader.finishNext(result: result)
        drainMainQueue()
        precondition(reader.latestRequest?.apps?.appsOffset == 512)
        precondition(reader.latestRequest?.apps?.membersOffset == 512)

        model.select(target)
        reader.finishNext(result: result)
        drainMainQueue()

        precondition(reader.latestRequest?.apps?.appsOffset == 0)
        precondition(reader.latestRequest?.apps?.membersOffset == 0)
        precondition(reader.latestRequest?.apps?.key == result.apps?.key)
        precondition(reader.latestRequest?.target == target.identity)
    }

    private static func appModelPayload(_ value: LiveAppsRead?) throws -> LiveAppsRead {
        guard let value else {
            throw RecordingError(message: "Completed Apps read did not install its payload")
        }

        return value
    }

    private static func appModelResult(_ apps: LiveAppsRead) throws -> ReadResult {
        let selected = apps.list.header.selected
        guard let wall = selected.wallNs?.value else { throw LiveAppDataError.invalidQuantity }
        let observation = Observation(run: selected.stamp.run.text, boot: selected.stamp.boot.text,
            publication: selected.stamp.publication.value, runTick: selected.runTick.value,
            wallNs: wall, windowLosses: Losses(publications: 0, events: 0, unknown: false))
        let second = wall / 1_000_000_000

        return ReadResult(catalog: Catalog(path: "/recording", firstSecond: second,
            lastSecond: second, windows: 0, unknownClockTicks: 0, tables: ["processes"],
            live: true, tailTicks: 1, collectorCompleted: false, latestObservation: observation),
            selection: Selection(second: second, selected: observation, alternatives: [observation],
                summary: nil, trends: [], contextSeconds: 900),
            page: nil, machinePages: [:], apps: apps)
    }

    @MainActor
    private static func entering_apps_retains_the_selected_live_publication() {
        // App navigation requests real totals without pausing or replacing the live selection.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext()
        drainMainQueue()
        let identity = model.selection?.selected?.identity

        model.enterApps()

        precondition(model.following)
        precondition(model.route == "Apps")
        precondition(model.selection?.selected?.identity == identity)
        precondition(reader.latestRequest?.live == true)
        precondition(reader.latestRequest?.apps != nil)
        precondition(reader.latestRequest?.target == nil)
    }

    @MainActor
    private static func starts_live_once_across_repeated_window_appearances() {
        // Reopening a window must not replace or duplicate the app's initial live request.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)

        model.startLive()
        model.startLive()

        precondition(model.following)
        precondition(model.isLiveSource)
        precondition(reader.readCount == 1)
        precondition(reader.cancellations == 0)
        precondition(reader.latestRequest?.live == true)
        precondition(reader.latestRequest?.second == nil)
        precondition(reader.latestRequest?.target == nil)
    }

    @MainActor
    private static func polls_without_projecting_metrics_while_showing_computer_inventory() {
        // Goal: inventory keeps ingestion active without decoding hidden chart projections.
        // Method: enter Computer after a completed poll, then return to Overview.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext(catalog: computerCatalog(collectorPID: 42))
        drainMainQueue()
        let selected = model.selection?.selected
        model.route = "Computer"

        model.pollLive()

        precondition(reader.readCount == 2)
        precondition(model.following)
        precondition(model.selection?.selected == selected)

        precondition(reader.latestRequest?.catalogOnly == true)
        reader.finishNext(result: ReadResult(catalog: computerCatalog(collectorPID: 42),
            selection: nil, page: nil, machinePages: [:], unchanged: true))
        drainMainQueue()

        model.enterOverview()

        precondition(reader.readCount == 3)
        precondition(reader.latestRequest?.catalogOnly == false)
        precondition(reader.latestRequest?.live == true)
        precondition(reader.latestRequest?.target == nil)
        reader.finishNext()
        drainMainQueue()
    }

    @MainActor
    private static func keeps_discovering_the_collector_while_showing_computer_inventory() {
        // Goal: opening inventory during startup does not strand collector discovery.
        // Method: poll from Computer before the catalog identifies its live producer.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext(catalog: computerCatalog(collectorPID: nil))
        drainMainQueue()
        model.route = "Computer"

        model.pollLive()

        precondition(reader.readCount == 2)
        precondition(model.following)
        reader.finishNext(catalog: computerCatalog(collectorPID: 42))
        drainMainQueue()
    }

    private static func computerCatalog(collectorPID: UInt32?, publication: UInt64 = 9) -> Catalog {
        Catalog(path: "/recording", firstSecond: 17, lastSecond: 17, windows: 0,
            unknownClockTicks: 0, tables: ["ticks"], live: true, tailTicks: 1,
            collectorCompleted: false, latestObservation: Observation(run: "run-0", boot: "boot",
                publication: publication, runTick: publication, wallNs: 17_000_000_000,
                windowLosses: nil), latestCollectorPid: collectorPID)
    }

    @MainActor
    private static func completes_collector_startup_while_showing_computer_inventory() {
        // Goal: inventory must not prevent the two-publication collector handshake.
        // Method: enter Computer after the first identity, then deliver a later publication.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        var instant = 0.0
        let collector = LiveCollectorSession(process: ControlledLiveCollector(),
            pollingEnabled: false, now: { instant }, onFailure: { model.cancel() })
        collector.start()
        model.startLive()
        reader.finishNext(catalog: computerCatalog(collectorPID: 42))
        drainMainQueue()
        collector.observe(identity: model.catalog!.latestObservation!.identity, collectorPID: 42,
            collectorCompleted: false)
        model.route = "Computer"

        model.pollLive()

        precondition(reader.readCount == 2)
        reader.finishNext(result: ReadResult(catalog: computerCatalog(collectorPID: 42, publication: 10),
            selection: nil, page: nil, machinePages: [:], unchanged: true))
        drainMainQueue()
        collector.observe(identity: model.catalog!.latestObservation!.identity, collectorPID: 42,
            collectorCompleted: false)
        instant = 21
        collector.step()

        precondition(collector.state == .running)
        precondition(model.following)
        precondition(reader.cancellations == 0)
    }

    @MainActor
    private static func preserves_paused_metric_identity_through_computer_inventory() {
        // Goal: viewing current inventory does not change a paused metric selection.
        // Method: pause an observation, navigate through Computer, then request Overview.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext()
        drainMainQueue()
        model.setLive(false)
        let selected = model.selection?.selected
        model.route = "Computer"

        model.pollLive()
        model.enterOverview()

        precondition(!model.following)
        precondition(model.selection?.selected == selected)
        precondition(reader.readCount == 2)
        precondition(reader.latestRequest?.target == selected?.identity)
        reader.finishNext()
        drainMainQueue()
    }

    @MainActor
    private static func returns_from_computer_to_metric_destination(_ title: String, paused: Bool) {
        // Goal: every route-only resource requests its proper projection when leaving inventory.
        // Method: use the same navigation action as the sidebar with the timer disabled.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext()
        drainMainQueue()
        if paused { model.setLive(false) }
        let identity = model.selection?.selected?.identity
        model.navigate(to: "Computer")

        model.navigate(to: title)

        precondition(model.route == title)
        precondition(model.following == !paused)
        precondition(reader.readCount == 2)
        precondition(reader.latestRequest?.catalogOnly == false)
        precondition(reader.latestRequest?.target == (paused ? identity : nil))
        precondition(model.selection?.selected?.identity == identity)
        reader.finishNext()
        drainMainQueue()
    }

    @MainActor
    private static func cancels_failed_startup_polling_with_reference_visible_and_resumes_on_retry() {
        // No mounted view may be necessary for failure to stop the actual model's read loop.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        var instant = 0.0
        let session = LiveCollectorSession(process: ControlledLiveCollector(), pollingEnabled: false,
            now: { instant }, onFailure: { if model.isLiveSource { model.cancel() } })
        model.startLive()
        session.start()
        model.showsReference = true
        instant = 20

        session.step()
        reader.finishNext()
        drainMainQueue()
        model.pollLive()

        precondition(!model.following)
        precondition(reader.cancellations == 1)
        precondition(reader.readCount == 1)

        session.retry()
        model.setLive(true)

        precondition(session.state == .starting)
        precondition(model.following)
        precondition(reader.readCount == 2)
    }

    @MainActor
    private static func keeps_paused_status_when_its_frozen_reading_ages() {
        // A frozen selected tick cannot establish that a still-running collector is stale.
        let reader = ControlledRecordingReader()
        var instant = Date(timeIntervalSince1970: 17)
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false,
            now: { instant })
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        model.setLive(false)

        instant = Date(timeIntervalSince1970: 21)

        precondition(model.liveStatus == "Paused")
        precondition(model.selection?.second == 17)
        precondition(model.isLiveSource)
    }

    @MainActor
    private static func emits_no_changes_for_an_identical_background_poll() {
        // An unchanged background read must not ask the native view tree to redraw.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false,
            now: { Date(timeIntervalSince1970: 17) })
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        var changes = 0
        let subscription = model.objectWillChange.sink { changes += 1 }
        defer { subscription.cancel() }

        model.pollLive()
        reader.finishNext(unchanged: true)
        drainMainQueue()

        precondition(changes == 0, "An identical background poll invalidated the native view")
        precondition(!model.busy)
        precondition(model.liveStatus == "Live")
    }

    @MainActor
    private static func publishes_catalog_changes_without_replacing_the_selected_payload() {
        // A seal or loss update remains visible even when its selected readings are unchanged.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        model.row = 1
        let original = model.catalog!
        let metadata = Catalog(path: original.path, firstSecond: original.firstSecond,
            lastSecond: original.lastSecond, windows: 2, unknownClockTicks: 0,
            tables: original.tables, live: true, tailTicks: 0, collectorCompleted: true,
            latestObservation: original.latestObservation,
            readerLosses: [ReaderLosses(run: "run-0", boot: "boot",
                losses: Losses(publications: 3, events: 2, unknown: true))])
        var changes = 0
        let subscription = model.objectWillChange.sink { changes += 1 }
        defer { subscription.cancel() }

        model.pollLive()
        reader.finishNext(unchanged: true, catalog: metadata)
        drainMainQueue()

        precondition(changes > 0)
        precondition(model.catalog?.windows == 2)
        precondition(model.catalog?.tailTicks == 0)
        precondition(model.liveStatus == "Collector stopped")
        precondition(model.readerLossNotices.count == 1)
        precondition(model.row == 1)
        precondition(model.page?.rows == [["1"], ["2"]])
    }

    @MainActor
    private static func publishes_clock_only_status_transitions_once() {
        // Time can cross the stale boundary without a new publication or catalog value.
        let reader = ControlledRecordingReader()
        var instant = Date(timeIntervalSince1970: 17)
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false,
            now: { instant })
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        var changes = 0
        let subscription = model.objectWillChange.sink { changes += 1 }
        defer { subscription.cancel() }

        instant = Date(timeIntervalSince1970: 21)
        model.pollLive()
        reader.finishNext(unchanged: true)
        drainMainQueue()

        precondition(model.liveStatus == "Waiting for collector")
        precondition(changes == 1)

        model.pollLive()
        reader.finishNext(unchanged: true)
        drainMainQueue()

        precondition(changes == 1, "A stable stale status kept invalidating the native view")
    }

    @MainActor
    private static func shows_foreground_loading_while_a_background_poll_is_being_canceled() {
        // A foreground choice is loading even before its canceled predecessor completes.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        model.pollLive()

        model.load(second: 16)

        precondition(model.busy, "Queued foreground work did not show its loading state")
        precondition(model.selection == nil)
        precondition(reader.readCount == 2)
        precondition(reader.cancellations == 1)
        reader.finishNext()
        drainMainQueue()
        precondition(model.busy)
        reader.finishNext()
        drainMainQueue()
        precondition(!model.busy)
        precondition(model.selection?.second == 16)
    }

    @MainActor
    private static func advances_visible_readings_after_a_changed_background_poll() {
        // An ordinary poll replaces the visible observation and its table atomically.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()

        model.pollLive()
        reader.finishNext(second: 18, publication: 10)
        drainMainQueue()

        precondition(model.selection?.second == 18)
        precondition(model.selection?.selected?.publication == 10)
        precondition(model.page?.observation.publication == 10)
        precondition(model.secondText == "18")
        precondition(model.following)
    }

    @MainActor
    private static func retains_poll_errors_until_success_without_republishing_identical_failures() {
        // A retry keeps the same error visible, then clears it when unchanged data recovers.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false,
            now: { Date(timeIntervalSince1970: 17) })
        let failure = RecordingError(message: "Live reader unavailable")
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        model.pollLive()
        reader.finishNext(failure: failure)
        drainMainQueue()
        var changes = 0
        let subscription = model.objectWillChange.sink { changes += 1 }
        defer { subscription.cancel() }

        model.pollLive()

        precondition(model.error == failure.message)
        reader.finishNext(failure: failure)
        drainMainQueue()
        precondition(changes == 0)

        model.pollLive()
        reader.finishNext(unchanged: true)
        drainMainQueue()

        precondition(model.error == nil)
        precondition(changes == 1)
        precondition(model.selection?.second == 17)
    }

    @MainActor
    private static func chart_selection_requests_and_retains_exact_identity() {
        // A chart click pauses following and carries identity independently of an index.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        let identity = RecordedObservationIdentity(run: "run-1", boot: "boot", publication: 9)

        model.select(RecordedSelectionTarget(second: 17, identity: identity))

        precondition(!model.following)
        precondition(reader.latestRequest?.target == identity)
        precondition(reader.latestRequest?.alternative == 0)
        precondition(model.selection == nil)
        reader.finishNext()
        drainMainQueue()
        precondition(model.selection?.selected?.identity == identity)
        model.chooseTable(1)
        precondition(reader.latestRequest?.target == identity)
        reader.finishNext()
        drainMainQueue()
        precondition(model.selection?.selected?.identity == identity)
    }

    @MainActor
    private static func unavailable_chart_identity_clears_readings_and_surfaces_error() {
        // An unavailable target cannot leave a sibling's row visible as the selected reading.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.load(second: 17)
        reader.finishNext()
        drainMainQueue()

        model.select(RecordedSelectionTarget(second: 17,
            identity: RecordedObservationIdentity(run: "gone", boot: "boot", publication: 9)))
        reader.finishNext()
        drainMainQueue()

        precondition(model.error == "The requested observation is no longer available.")
        precondition(model.selection == nil)
        precondition(model.page == nil)
        precondition(model.machinePages.isEmpty)
    }

    @MainActor
    private static func entering_component_retains_the_selected_live_publication(
        _ component: RecordedComponent
    ) {
        // Route-only panes reuse the newest selected publication without issuing a read.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        let selected = model.selection?.selected?.identity

        model.enterComponent(component)

        precondition(model.route == component.rawValue)
        precondition(model.following)
        precondition(model.selection?.selected?.identity == selected)
        precondition(reader.readCount == 1)
        precondition(!model.busy)
    }

    @MainActor
    private static func entering_cpu_fetches_owned_tables_without_pausing_live() {
        // CPU needs process and machine pages while continuing to follow the live source.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        let selected = model.selection?.selected?.identity

        model.enterComponent(.cpu)

        precondition(model.following)
        precondition(model.selection?.selected?.identity == selected)
        precondition(reader.latestRequest?.second == nil)
        precondition(reader.latestRequest?.target == nil)
        precondition(reader.latestRequest?.table == 7)
        precondition(reader.latestRequest?.includeMachine == true)
        reader.finishNext()
        drainMainQueue()
        precondition(model.machinePages.count == MachineSource.allCases.count)
    }

    @MainActor
    private static func pins_visible_identity_when_pausing_an_active_cpu_read() {
        // A latest CPU read can finish after pause, so complete it with a newer publication.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        let selected = model.selection!.selected!.identity
        model.enterComponent(.cpu)

        model.setLive(false)
        reader.finishNext(second: 18, publication: 10)
        drainMainQueue()

        precondition(model.selection?.second == 17, "Pause replaced visible second 17")
        precondition(model.selection?.selected?.identity == selected)
        precondition(!model.following)
        precondition(reader.latestRequest?.second == 17)
        precondition(reader.latestRequest?.target == selected)
        precondition(reader.latestRequest?.includeMachine == true)
        reader.finishNext()
        drainMainQueue()
        precondition(!model.busy)
        precondition(model.page?.name == "processes")
        precondition(model.page?.observation.identity == selected)
        precondition(model.machinePages.count == MachineSource.allCases.count)
        precondition(model.machinePages.values.allSatisfy { $0.observation.identity == selected })
    }

    @MainActor
    private static func pins_visible_identity_when_pausing_a_pending_cpu_read() {
        // CPU entry waits behind a poll; pause must replace that pending latest request too.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        let selected = model.selection!.selected!.identity
        model.pollLive()
        model.enterComponent(.cpu)

        model.setLive(false)
        reader.finishNext(second: 18, publication: 10)
        drainMainQueue()

        precondition(model.selection?.second == 17)
        precondition(model.selection?.selected?.identity == selected)
        precondition(!model.following)
        precondition(reader.latestRequest?.second == 17, "Pause retained the pending latest read")
        precondition(reader.latestRequest?.target == selected)
        precondition(reader.latestRequest?.includeMachine == true)
        reader.finishNext()
        drainMainQueue()
        precondition(!model.busy)
        precondition(model.page?.name == "processes")
        precondition(model.page?.observation.identity == selected)
        precondition(model.machinePages.count == MachineSource.allCases.count)
        precondition(model.machinePages.values.allSatisfy { $0.observation.identity == selected })
    }

    @MainActor
    private static func cancels_latest_work_when_pausing_without_visible_readings() {
        // With no visible observation to pin, pause must discard the initial latest result.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)

        model.setLive(false)
        reader.finishNext(second: 18, publication: 10)
        drainMainQueue()

        precondition(!model.following)
        precondition(!model.busy)
        precondition(model.selection == nil)
        precondition(model.page == nil)
        precondition(model.machinePages.isEmpty)
        precondition(reader.readCount == 1)
        precondition(reader.cancellations == 1)
    }

    @MainActor
    private static func retains_exact_paused_identity_through_cpu_and_paging() {
        // Table requests retain identity until an explicit second, alternative or live choice.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.load(second: 17, alternative: 1)
        reader.finishNext()
        drainMainQueue()
        let selected = model.selection!.selected!.identity

        model.enterComponent(.cpu)

        precondition(reader.latestRequest?.target == selected)
        reader.finishNext()
        drainMainQueue()
        model.chooseTable(1)
        precondition(reader.latestRequest?.target == selected)
        reader.finishNext()
        drainMainQueue()
        model.load(offset: 512, refresh: true)
        precondition(reader.latestRequest?.target == selected)
        reader.finishNext()
        drainMainQueue()
        model.load(second: 17)
        precondition(reader.latestRequest?.target == nil)
        reader.finishNext()
        drainMainQueue()
        model.setLive(true)
        precondition(reader.latestRequest?.target == nil)
        reader.finishNext()
        drainMainQueue()
    }

    @MainActor
    private static func entering_memory_retains_the_selected_live_publication() {
        // Resource navigation reuses the selected publication without scheduling work.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        let selected = model.selection?.selected
        let reads = reader.readCount

        model.enterMemory()

        precondition(model.route == "Memory")
        precondition(model.following)
        precondition(model.selection?.selected?.run == selected?.run)
        precondition(model.selection?.selected?.boot == selected?.boot)
        precondition(model.selection?.selected?.publication == selected?.publication)
        precondition(reader.readCount == reads)
        precondition(!model.busy)
    }

    @MainActor
    private static func opening_a_recording_exits_captured_reference_mode() {
        // Opening actual history must replace the captured-reference source on screen.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/old", reader: reader, pollingEnabled: false,
            showsReference: true)
        model.chooseRecording(path: "/chosen")
        precondition(!model.showsReference)
        precondition(model.path == "/chosen")
        precondition(reader.latestRequest?.path == "/chosen")
        precondition(reader.latestRequest?.refresh == true)
        reader.finishNext()
        drainMainQueue()
        precondition(model.selection?.second == 17)
    }

    @MainActor
    private static func reads_the_latest_real_publication_after_rollback_and_repetition(
        path: String
    ) throws {
        // Goal: the real bridge follows publication identity across repeated and older walls.
        // Method: read the registered three-tick Rust fixture through RecordingWorker.
        let reader = RecordingWorker()
        let completion = NativeReadCompletion()
        reader.read(ReadRequest(path: path, second: nil, alternative: 0, table: 1, offset: 0,
            refresh: false, machineOffsets: [:], includeMachine: false, live: true)) {
            result in completion.complete(result)
        }
        precondition(completion.ready.wait(timeout: .now() + 5) == .success,
                     "Real recording worker exceeded five seconds")
        guard let result = completion.take() else {
            throw RecordingError(message: "Real recording worker returned no result")
        }
        let response = try result.get()

        precondition(response.catalog.lastSecond == 1000)
        guard response.selection?.second == 900 else {
            throw RecordingError(message: "Worker chose the maximum wall second instead of 900")
        }
        guard response.selection?.selected?.publication == 3 else {
            throw RecordingError(message: "Worker chose the default alternative instead of publication 3")
        }
        precondition(response.selection?.alternatives.map(\.publication) == [2, 3])
        precondition(response.selection?.contextSeconds == 900)
        precondition(response.selection?.trends.count == 2)
        precondition(response.selection?.selected?.run == response.catalog.latestObservation?.run)
        precondition(response.selection?.selected?.boot == response.catalog.latestObservation?.boot)
        precondition(response.page?.observation.publication == 3)
        precondition(response.page?.observation.wallNs == 900_000_000_000)
        precondition(response.page?.rows == [["3", "333", "0", "0", "0", "0", "0", "0", "0",
            "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0"]])
        let target = RecordedObservationIdentity(run: response.selection!.selected!.run,
            boot: response.selection!.selected!.boot, publication: 2)
        let pinnedWithoutSecond = try nativeRead(reader, request: ReadRequest(path: path,
            second: nil, alternative: 1, table: 1, offset: 0, refresh: false,
            machineOffsets: [:], includeMachine: false, live: true, target: target))
        precondition(pinnedWithoutSecond.selection?.selected?.identity == target)
        precondition(pinnedWithoutSecond.page?.rows[0][1] == "222")
        let pinned = try nativeRead(reader, request: ReadRequest(path: path, second: 900,
            alternative: 1, table: 1, offset: 0, refresh: false, machineOffsets: [:],
            includeMachine: false, live: true, target: target))
        precondition(pinned.selection?.selected?.identity == target,
            "Expected requested publication 2, received \(String(describing: pinned.selection?.selected?.publication))")
        precondition(pinned.page?.observation.identity == target)
        precondition(pinned.page?.rows[0][1] == "222")
        let earlier = RecordedObservationIdentity(run: target.run, boot: target.boot, publication: 1)
        let differentSecond = try nativeRead(reader, request: ReadRequest(path: path, second: 1000,
            alternative: 1, table: 1, offset: 0, refresh: false, machineOffsets: [:],
            includeMachine: false, live: true, target: earlier))
        precondition(differentSecond.selection?.selected?.identity == earlier)
        precondition(differentSecond.page?.rows[0][1] == "111")
        let missing = RecordedObservationIdentity(run: target.run, boot: target.boot,
            publication: 999)
        let failure = nativeFailure(reader, request: ReadRequest(path: path, second: 900,
            alternative: 0, table: 1, offset: 0, refresh: false, machineOffsets: [:],
            includeMachine: false, live: true, target: missing))
        precondition(failure.message == "The requested observation is no longer available.")
        print("Real RecordingWorker rollback and repeated-second fixture passed")
        print("Real RecordingWorker pins requested identity and refuses an unavailable sibling")
    }

    @MainActor
    private static func omits_unrequested_raw_pages_without_poisoning_later_queries(
        path: String
    ) throws {
        // Goal: chart requests omit raw rows and unchanged-source inspection still loads them.
        // Method: alternate the real worker's projection on one retained live publication.
        let reader = RecordingWorker()

        let chart = try nativeRead(reader, request: rawPageRequest(path: path, includePage: false))

        precondition(chart.page == nil, "Chart-only projection decoded an unrequested raw page")
        precondition(chart.selection?.selected?.publication == 3)
        let raw = try nativeRead(reader, request: rawPageRequest(path: path, includePage: true))
        precondition(!raw.unchanged, "Raw inspection reused a chart-only cached projection")
        precondition(raw.page?.name == "memory")
        precondition(raw.page?.rows.first?[1] == "333")
        precondition(raw.page?.observation.identity == chart.selection?.selected?.identity)
        let restored = try nativeRead(reader,
            request: rawPageRequest(path: path, includePage: false))
        precondition(!restored.unchanged, "Chart projection retained cached raw rows")
        precondition(restored.page == nil)
        precondition(restored.selection?.selected?.identity == raw.selection?.selected?.identity)
        print("Real RecordingWorker omits raw chart pages and refreshes changed projections")
    }

    private static func rawPageRequest(path: String, includePage: Bool) -> ReadRequest {
        ReadRequest(path: path, second: nil, alternative: 0, table: 1, offset: 0,
            refresh: false, machineOffsets: [:], includeMachine: false,
            includePage: includePage, live: true, poll: true)
    }

    @MainActor
    private static func polls_catalog_without_reusing_or_decoding_a_metric_projection(
        path: String
    ) throws {
        // Goal: inventory polls ingest publications without decoding any hidden metric payload.
        // Method: alternate catalog-only and chart requests through the real worker.
        let reader = RecordingWorker()
        let chart = try nativeRead(reader, request: rawPageRequest(path: path, includePage: false))
        let inventory = ReadRequest(path: path, second: nil, alternative: 0, table: 1, offset: 0,
            refresh: false, machineOffsets: [:], includeMachine: true, includePage: true,
            includeFans: true, live: true, poll: true, catalogOnly: true)

        let result = try nativeRead(reader, request: inventory)

        precondition(result.catalog.latestObservation == chart.catalog.latestObservation)
        precondition(result.selection == nil)
        precondition(result.page == nil)
        precondition(result.machinePages.isEmpty)
        precondition(result.fans == nil)
        precondition(result.apps == nil)
        precondition(result.unchanged)
        let restored = try nativeRead(reader,
            request: rawPageRequest(path: path, includePage: false))
        precondition(!restored.unchanged)
        precondition(restored.selection?.selected?.identity == chart.selection?.selected?.identity)
        print("Real RecordingWorker inventory polls keep catalog ingestion and omit hidden projections")
    }

    @MainActor
    private static func nativeRead(_ reader: RecordingWorker, request: ReadRequest) throws -> ReadResult {
        let completion = NativeReadCompletion()
        reader.read(request) { result in completion.complete(result) }
        precondition(completion.ready.wait(timeout: .now() + 5) == .success,
            "Real recording worker exceeded five seconds")
        guard let result = completion.take() else {
            throw RecordingError(message: "Real recording worker returned no result")
        }

        return try result.get()
    }

    @MainActor
    private static func nativeFailure(_ reader: RecordingWorker,
                                      request: ReadRequest) -> RecordingError {
        do {
            let response = try nativeRead(reader, request: request)
            preconditionFailure("Unavailable identity selected publication \(String(describing: response.selection?.selected?.publication))")
        } catch let failure as RecordingError {
            return failure
        } catch {
            preconditionFailure("Unexpected native error: \(error)")
        }
    }

    @MainActor
    private static func reports_reader_losses_without_a_selected_tick() {
        // Catalog losses survive even when an overwrite leaves no selectable observation.
        let model = RecordingModel(path: "/recording", reader: ControlledRecordingReader(), pollingEnabled: false)
        model.catalog = Catalog(path: "/recording", firstSecond: nil, lastSecond: nil,
            windows: 0, unknownClockTicks: 0, tables: [], live: true,
            readerLosses: [ReaderLosses(run: "run", boot: "boot",
                losses: Losses(publications: 7, events: 3, unknown: true))])
        precondition(model.selection == nil)
        precondition(model.readerLossNotices == ["Live reader · run run: 7 missed publications, 3 missed events; additional loss is unknown."])
    }

    @MainActor
    private static func opens_raw_readings_at_the_paused_chart_observation_without_polling() {
        // Goal: explicit raw inspection loads the same paused publication without a timer.
        // Method: complete a chart-only result, pause, then use the menu's model operation.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        let selected = Observation(run: "run-0", boot: "boot", publication: 9, runTick: 9,
            wallNs: 17_000_000_000, windowLosses: nil)
        let catalog = Catalog(path: "/recording", firstSecond: 17, lastSecond: 17,
            windows: 0, unknownClockTicks: 0, tables: [], live: true,
            latestObservation: selected)
        let selection = Selection(second: 17, selected: selected, alternatives: [selected],
            summary: nil, trends: [], contextSeconds: 900)
        model.startLive()
        reader.finishNext(result: ReadResult(catalog: catalog, selection: selection, page: nil,
            machinePages: [:]))
        drainMainQueue()
        model.setLive(false)

        model.enterRawReadings()

        precondition(reader.readCount == 2, "Paused raw entry did not request its projection")
        precondition(reader.latestRequest?.includePage == true)
        precondition(reader.latestRequest?.target == selected.identity)
        precondition(reader.latestRequest?.second == 17)
        precondition(model.route == "Recorded readings")
        precondition(!model.following)
        reader.finishNext()
        drainMainQueue()
        precondition(model.page?.observation.identity == selected.identity)
        precondition(model.selection?.selected?.identity == selected.identity)
        precondition(!model.following)
    }

    @MainActor
    private static func requests_live_chart_without_a_raw_page(_ route: String) {
        // Goal: live chart destinations request readings without a hidden raw process page.
        // Method: inspect the real model's request at its owned reader boundary.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext()
        drainMainQueue()

        model.route = route
        model.pollLive()

        precondition(reader.latestRequest?.includePage == false, route)
        precondition(reader.latestRequest?.includeFans == (route == "Overview"), route)
        precondition(reader.latestRequest?.live == true, route)
        precondition(reader.latestRequest?.second == nil, route)
    }

    @MainActor
    private static func retains_raw_pages_for_live_inspection_and_saved_sources() {
        // Goal: skipping chart-only rows preserves both live raw inspection and saved callers.
        // Method: switch the real model's destination and source across completed reads.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.startLive()
        reader.finishNext()
        drainMainQueue()

        model.route = "Recorded readings"
        model.pollLive()

        precondition(reader.latestRequest?.includePage == true)
        reader.finishNext()
        drainMainQueue()
        model.enterProcesses()
        precondition(reader.latestRequest?.includePage == true)
        reader.finishNext()
        drainMainQueue()
        model.chooseRecording(path: "/saved")
        precondition(reader.latestRequest?.includePage == true)
        precondition(reader.latestRequest?.live == false)
    }

    @MainActor
    private static func starts_live_without_building_a_poll_queue() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)

        model.setLive(true)
        model.pollLive()
        model.pollLive()

        precondition(model.following)
        precondition(reader.readCount == 1)
        precondition(reader.latestRequest?.live == true)
        precondition(reader.latestRequest?.second == nil)
        precondition(reader.cancellations == 0)
        reader.finishNext()
        drainMainQueue()
        model.pollLive()
        precondition(reader.readCount == 2)
        precondition(reader.latestRequest?.poll == true)
        model.pollLive()
        model.pollLive()
        precondition(reader.readCount == 2)
        precondition(reader.cancellations == 0)
        reader.finishNext(unchanged: true)
        drainMainQueue()
        precondition(reader.readCount == 2)
    }

    @MainActor
    private static func preserves_visible_readings_when_a_live_poll_is_unchanged() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        model.row = 1

        model.pollLive()
        precondition(model.selection?.second == 17)
        precondition(model.page != nil)
        reader.finishNext(unchanged: true)
        drainMainQueue()

        precondition(model.selection?.second == 17)
        precondition(model.page?.rows == [["1"], ["2"]])
        precondition(model.row == 1)
        precondition(!model.busy)
    }

    @MainActor
    private static func keeps_live_tail_access_when_following_is_paused() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()

        model.setLive(false)
        model.load(second: 16)
        precondition(reader.latestRequest?.live == true)
        reader.finishNext()
        drainMainQueue()
        model.pollLive()

        precondition(!model.following)
        precondition(model.selection?.second == 16)
        precondition(reader.readCount == 2)
    }

    @MainActor
    private static func keeps_saved_recordings_on_the_existing_default_path() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)

        model.load()
        reader.finishNext()
        drainMainQueue()
        model.pollLive()

        precondition(!model.following)
        precondition(reader.latestRequest?.live == false)
        precondition(reader.readCount == 1)
        precondition(model.liveStatus == "Recorded")
    }

    @MainActor
    private static func reports_a_stale_collector_using_the_injected_clock() {
        let reader = ControlledRecordingReader()
        var instant = Date(timeIntervalSince1970: 17)
        let model = RecordingModel(path: "/recording", reader: reader,
            pollingEnabled: false, now: { instant })
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        precondition(model.liveStatus == "Live")

        instant = Date(timeIntervalSince1970: 21)

        precondition(model.liveStatus == "Waiting for collector")
    }

    @MainActor
    private static func reports_live_from_a_catalog_second_without_an_observation() {
        // A whole-second catalog clock must be converted numerically, not as Double bits.
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false,
            now: { Date(timeIntervalSince1970: 1_792_252_314) })
        model.setLive(true)
        let catalog = Catalog(path: "/recording", firstSecond: 1_792_252_314,
            lastSecond: 1_792_252_314, windows: 0, unknownClockTicks: 0,
            tables: [], live: true, tailTicks: 1, collectorCompleted: false)

        reader.finishNext(catalog: catalog)
        drainMainQueue()

        precondition(model.liveStatus == "Live", "A current catalog second was treated as stale")
    }

    @MainActor
    private static func freezes_a_pending_live_poll_when_following_is_paused() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader, pollingEnabled: false)
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()
        model.pollLive()

        model.setLive(false)
        reader.finishNext(second: 18)
        drainMainQueue()

        precondition(model.selection?.second == 17)
        precondition(!model.following)
        precondition(reader.latestRequest?.second == 17)
        precondition(reader.latestRequest?.target == model.selection?.selected?.identity)
        reader.finishNext()
        drainMainQueue()
        precondition(model.selection?.second == 17)
        precondition(!model.busy)
    }

    @MainActor
    private static func reports_a_future_tick_clock_without_trusting_the_catalog_maximum() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader,
            pollingEnabled: false, now: { Date(timeIntervalSince1970: 10) })
        model.setLive(true)
        reader.finishNext()
        drainMainQueue()

        precondition(model.liveStatus == "Clock changed")
    }

    @MainActor
    private static func applies_native_table_selection_values_and_theme_changes() {
        let observation = Observation(run: "run", boot: "boot", publication: 9, runTick: 9,
            wallNs: 17_000_000_000,
            windowLosses: Losses(publications: 0, events: 0, unknown: false))
        let page = TablePage(name: "processes", columns: ["user_ns"],
            rows: [["9007199254740993"], [nil]], offset: 0, total: 2, observation: observation)
        var selected: Int?
        let coordinator = RecordedTable.Coordinator(
            selection: Binding(get: { selected }, set: { selected = $0 }))
        let table = NSTableView()
        table.delegate = coordinator
        table.dataSource = coordinator
        let initial = FlightdeckTheme()
        coordinator.apply(table, page: page, fields: page.columns, theme: initial)
        table.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
        precondition(selected == 1)
        let column = table.tableColumns[0]
        let exact = coordinator.tableView(table, viewFor: column, row: 0) as? NSTextField
        precondition(exact?.stringValue == "9007199254740993")
        precondition(exact?.accessibilityValue() as? String == "9007199254740993")
        let unavailable = coordinator.tableView(table, viewFor: column, row: 1) as? NSTextField
        precondition(unavailable?.stringValue == "Unavailable")

        var replacement = initial
        replacement.tableFontSize = 19
        replacement.text = .yellow
        replacement.panel = .black
        coordinator.apply(table, page: page, fields: page.columns, theme: replacement)

        let changed = coordinator.tableView(table, viewFor: column, row: 0) as? NSTextField
        precondition(changed?.font?.pointSize == 19)
        precondition(column.headerCell.font?.pointSize == 19)
        precondition(changed?.textColor == NSColor(replacement.text))
        precondition(table.backgroundColor == NSColor(replacement.panel))
        precondition(selected == 1)
    }

    @MainActor
    private static func enters_processes_from_another_source() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.table = 1

        model.enterProcesses()
        reader.finishNext()
        drainMainQueue()

        precondition(model.route == "CPU & processes")
        precondition(model.page?.name == "processes")
    }

    @MainActor
    private static func retains_refresh_after_a_later_selection() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.load(second: 10)

        model.load(second: 10, refresh: true)
        model.load(second: 11)
        reader.finishNext()
        drainMainQueue()
        reader.finishNext()
        drainMainQueue()

        precondition(model.selection?.second == 11)
        precondition(reader.refreshed)
    }

    @MainActor
    private static func ignores_a_completed_stale_selection() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.load(second: 10)

        model.load(second: 11)
        reader.finishNext()
        drainMainQueue()

        precondition(model.selection == nil)
        precondition(model.busy)
        reader.finishNext()
        drainMainQueue()
        precondition(model.selection?.second == 11)
    }

    @MainActor
    private static func preserves_process_and_observation_during_machine_paging() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.route = "CPU & processes"
        model.load(second: 17, alternative: 1, offset: 1024)
        reader.finishNext()
        drainMainQueue()
        model.row = 1

        model.pageMachine(.interfaces, offset: 512)
        precondition(model.page?.offset == 1024)
        precondition(model.row == 1)
        reader.finishNext()
        drainMainQueue()
        model.pageMachine(.interfaces, offset: 1024)
        reader.finishNext()
        drainMainQueue()

        precondition(model.selection?.second == 17)
        precondition(model.selection?.selected?.run == "run-1")
        precondition(model.page?.name == "processes")
        precondition(model.page?.offset == 1024)
        precondition(model.row == 1)
        precondition(model.machinePages[.interfaces]?.offset == 1024)
    }

    @MainActor
    private static func clears_old_readings_before_a_changed_second_completes() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.route = "CPU & processes"
        model.load(second: 10)
        reader.finishNext()
        drainMainQueue()

        model.load(second: 12)

        precondition(model.selection == nil)
        precondition(model.page == nil)
        precondition(model.machinePages.isEmpty)
        reader.finishNext(missingSecond: true)
        drainMainQueue()
        precondition(model.selection?.selected == nil)
        precondition(model.page == nil)
        precondition(model.machinePages.isEmpty)
    }

    @MainActor
    private static func clears_the_previous_source_before_a_new_source_completes() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.load(second: 10)
        reader.finishNext()
        drainMainQueue()

        model.chooseTable(1)

        precondition(model.page == nil)
        reader.finishNext()
        drainMainQueue()
        precondition(model.page?.name == "memory")
    }

    private static func preserves_exact_recorded_cell_strings_and_unavailability() throws {
        let data = Data(#"""
            {"value":{"name":"processes",
              "columns":["user_ns","pid_min","pid_max","resident_bytes"],
              "rows":[["9007199254740993","-2147483648","2147483647",null]],
              "offset":0,"total":1,
              "observation":{"run":"run","boot":"boot","publication":9,"run_tick":9,
                "wall_ns":17000000000,
                "window_losses":{"publications":0,"events":0,"unknown":false}}}}
            """#.utf8)

        let page: TablePage = try decodeRecordingResponse(data)

        precondition(page.rows == [["9007199254740993", "-2147483648", "2147483647", nil]])
    }

    private static func reports_native_query_errors() {
        let data = Data(#"{"error":"recording unavailable"}"#.utf8)

        let result = Result { () throws -> TablePage in try decodeRecordingResponse(data) }

        assertRecordingFailure(result, message: "recording unavailable")
    }

    private static func assertRecordingFailure(_ result: Result<TablePage, any Error>,
                                               message: String) {
        switch result {
        case .failure(let error):
            precondition(error is RecordingError)
            precondition(error.localizedDescription == message)
        case .success: preconditionFailure("A native error envelope decoded as a table")
        }
    }

    @MainActor
    private static func cancels_a_superseded_query_before_submitting_its_replacement() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.load(second: 10)

        model.load(second: 11)

        precondition(reader.cancellations == 1)
        reader.finishNext()
        drainMainQueue()
        reader.finishNext()
        drainMainQueue()
        precondition(model.selection?.second == 11)
    }

    @MainActor
    private static func cancels_without_scheduling_another_read() {
        let reader = ControlledRecordingReader()
        let model = RecordingModel(path: "/recording", reader: reader)
        model.load(second: 10)

        model.cancel()
        reader.finishNext()
        drainMainQueue()

        precondition(model.selection == nil)
        precondition(!model.busy)
        precondition(reader.cancellations == 1)
    }

    @MainActor
    private static func surfaces_a_failed_query_cancellation() {
        let reader = ControlledRecordingReader(
            cancellationFailure: RecordingError(message: "cancel unavailable"))
        let model = RecordingModel(path: "/recording", reader: reader)
        model.load(second: 10)

        model.load(second: 11)
        reader.finishNext()
        drainMainQueue()

        precondition(model.error == "cancel unavailable")
        precondition(model.selection == nil)
        precondition(!model.busy)
    }

    private static func distinguishes_publications_from_different_runs_and_boots() throws {
        let data = Data(#"""
            {"value":[
              {"run":"first","boot":"boot","publication":9,"wall_ns":1,"segment":1},
              {"run":"second","boot":"boot","publication":9,"wall_ns":2,"segment":2},
              {"run":"first","boot":"other","publication":9,"wall_ns":3,"segment":3}]}
            """#.utf8)

        let points: [Metric] = try decodeRecordingResponse(data)

        precondition(Set(points.map(\.id)).count == 3)
    }

    @MainActor
    private static func drainMainQueue() {
        var finished = false
        DispatchQueue.main.async {
            finished = true
            CFRunLoopStop(CFRunLoopGetMain())
        }
        CFRunLoopRunInMode(.defaultMode, 5, false)
        precondition(finished, "Main queue failed to deliver recording state")
    }
}

private final class NativeReadCompletion: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<ReadResult, RecordingError>?
    let ready = DispatchSemaphore(value: 0)

    func complete(_ value: Result<ReadResult, RecordingError>) {
        lock.withLock { result = value }
        ready.signal()
    }

    func take() -> Result<ReadResult, RecordingError>? { lock.withLock { result } }
}

private final class ControlledRecordingReader: RecordingReading, @unchecked Sendable {
    private let lock = NSLock()
    private var pending: (ReadRequest, @Sendable (Result<ReadResult, RecordingError>) -> Void)?
    private var didRefresh = false
    private var submitted: UInt64 = 0
    private var mostRecent: ReadRequest?
    private var cancellationCount: UInt64 = 0
    private let cancellationFailure: RecordingError?

    init(cancellationFailure: RecordingError? = nil) {
        self.cancellationFailure = cancellationFailure
    }

    var readCount: UInt64 { lock.withLock { submitted } }
    var latestRequest: ReadRequest? { lock.withLock { mostRecent } }
    var cancellations: UInt64 { lock.withLock { cancellationCount } }
    var refreshed: Bool { lock.withLock { didRefresh } }

    func read(_ request: ReadRequest, completion: @escaping @Sendable
              (Result<ReadResult, RecordingError>) -> Void) {
        lock.withLock {
            precondition(pending == nil)
            pending = (request, completion)
            submitted += 1
            mostRecent = request
            didRefresh = didRefresh || request.refresh
        }
    }

    func cancel() throws {
        try lock.withLock {
            cancellationCount += 1
            if let cancellationFailure { throw cancellationFailure }
        }
    }

    func finishNext(missingSecond: Bool = false, unchanged: Bool = false,
                    second: UInt64? = nil, publication: UInt64 = 9, catalog: Catalog? = nil,
                    failure: RecordingError? = nil, result: ReadResult? = nil) {
        let next = lock.withLock {
            precondition(pending != nil)
            let next = pending
            pending = nil
            return next
        }
        guard let (request, completion) = next else { preconditionFailure() }
        if let failure {
            completion(.failure(failure))
            return
        }
        if let result {
            completion(.success(result))
            return
        }

        do {
            let result = try response(request, missingSecond: missingSecond, second: second,
                publication: publication)
            completion(.success(ReadResult(catalog: catalog ?? result.catalog, selection: result.selection,
                page: result.page, machinePages: result.machinePages, unchanged: unchanged)))
        } catch let failure as RecordingError {
            completion(.failure(failure))
        } catch {
            preconditionFailure("Unexpected controlled reader error: \(error)")
        }
    }

    private func response(_ request: ReadRequest, missingSecond: Bool,
                          second: UInt64? = nil, publication: UInt64) throws -> ReadResult {
        let observations = (0..<2).map { index in
            Observation(run: "run-\(index)", boot: "boot", publication: publication,
                        runTick: publication,
                        wallNs: (second ?? request.second ?? 17) * 1_000_000_000,
                        windowLosses: Losses(publications: 0, events: 0, unknown: false))
        }
        let observation: Observation
        if let target = request.target {
            guard let exact = observations.first(where: { $0.identity == target }) else {
                throw RecordingError(message: "The requested observation is no longer available.")
            }

            observation = exact
        } else {
            observation = observations[Int(request.alternative)]
        }
        let selection = Selection(second: second ?? request.second ?? 17, selected: observation,
                                  alternatives: observations, summary: nil, trends: [],
                                  contextSeconds: 900)
        let tables = ["ticks", "memory", "cpus", "interfaces", "cores", "states", "events", "processes"]
        let catalog = Catalog(path: request.path, firstSecond: 10,
                              lastSecond: request.live ? 99_999 : 17, windows: 1,
                              unknownClockTicks: 0, tables: tables, live: request.live,
                              tailTicks: request.live ? 1 : nil, collectorCompleted: false,
                              latestObservation: request.live ? observation : nil)
        var machinePages: [MachineSource: TablePage] = [:]
        if request.includeMachine {
            for source in MachineSource.allCases {
                machinePages[source] = TablePage(name: source.rawValue, columns: ["publication"],
                    rows: [[String(publication)]], offset: request.machineOffsets[source] ?? 0, total: 2048,
                    observation: observation)
            }
        }
        let page = TablePage(name: tables[Int(request.table)], columns: ["pid"],
                             rows: [["1"], ["2"]], offset: request.offset, total: 2048,
                             observation: observation)
        if missingSecond {
            let missing = Selection(second: selection.second, selected: nil, alternatives: [],
                                    summary: nil, trends: [], contextSeconds: 900)
            return ReadResult(catalog: catalog, selection: missing, page: nil, machinePages: [:])
        }
        return ReadResult(catalog: catalog, selection: selection, page: page,
                          machinePages: machinePages)
    }
}
