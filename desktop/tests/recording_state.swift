import AppKit
import Foundation
import SwiftUI

@main
struct RecordingStateChecks {
    @MainActor
    static func main() throws {
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--live-fixture" {
            try reads_the_latest_real_publication_after_rollback_and_repetition(
                path: CommandLine.arguments[2])
            return
        }
        precondition(CommandLine.arguments.count == 1, "Unknown native check arguments")
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
        preserves_visible_readings_when_a_live_poll_is_unchanged()
        keeps_live_tail_access_when_following_is_paused()
        keeps_saved_recordings_on_the_existing_default_path()
        reports_a_stale_collector_using_the_injected_clock()
        freezes_a_pending_live_poll_when_following_is_paused()
        reports_a_future_tick_clock_without_trusting_the_catalog_maximum()
        reports_reader_losses_without_a_selected_tick()
        opening_a_recording_exits_captured_reference_mode()
        print("22 native recording state and table checks passed")
        try runReferenceDataChecks()
        runDashboardChecks()
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
        precondition(response.selection?.selected?.run == response.catalog.latestObservation?.run)
        precondition(response.selection?.selected?.boot == response.catalog.latestObservation?.boot)
        precondition(response.page?.observation.publication == 3)
        precondition(response.page?.observation.wallNs == 900_000_000_000)
        precondition(response.page?.rows == [["3", "333", "0", "0", "0", "0", "0", "0", "0",
            "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0"]])
        print("Real RecordingWorker rollback and repeated-second fixture passed")
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
        reader.finishNext(unchanged: true)
        drainMainQueue()
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
                    second: UInt64? = nil) {
        let next = lock.withLock {
            precondition(pending != nil)
            let next = pending
            pending = nil
            return next
        }
        guard let (request, completion) = next else { preconditionFailure() }
        let result = response(request, missingSecond: missingSecond, second: second)
        completion(.success(ReadResult(catalog: result.catalog, selection: result.selection,
            page: result.page, machinePages: result.machinePages, unchanged: unchanged)))
    }

    private func response(_ request: ReadRequest, missingSecond: Bool,
                          second: UInt64? = nil) -> ReadResult {
        let observations = (0..<2).map { index in
            Observation(run: "run-\(index)", boot: "boot", publication: 9, runTick: 9,
                        wallNs: (second ?? request.second ?? 17) * 1_000_000_000,
                        windowLosses: Losses(publications: 0, events: 0, unknown: false))
        }
        let observation = observations[Int(request.alternative)]
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
                    rows: [["9"]], offset: request.machineOffsets[source] ?? 0, total: 2048,
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
