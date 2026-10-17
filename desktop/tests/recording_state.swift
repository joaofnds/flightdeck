import AppKit
import Foundation
import SwiftUI

@main
struct RecordingStateChecks {
    @MainActor
    static func main() throws {
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
        print("13 native recording state and table checks passed")
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

private final class ControlledRecordingReader: RecordingReading, @unchecked Sendable {
    private let lock = NSLock()
    private var pending: (ReadRequest, @Sendable (Result<ReadResult, RecordingError>) -> Void)?
    private var didRefresh = false
    private var cancellationCount: UInt64 = 0
    private let cancellationFailure: RecordingError?

    init(cancellationFailure: RecordingError? = nil) {
        self.cancellationFailure = cancellationFailure
    }

    var cancellations: UInt64 { lock.withLock { cancellationCount } }
    var refreshed: Bool { lock.withLock { didRefresh } }

    func read(_ request: ReadRequest, completion: @escaping @Sendable
              (Result<ReadResult, RecordingError>) -> Void) {
        lock.withLock {
            precondition(pending == nil)
            pending = (request, completion)
            didRefresh = didRefresh || request.refresh
        }
    }

    func cancel() throws {
        try lock.withLock {
            cancellationCount += 1
            if let cancellationFailure { throw cancellationFailure }
        }
    }

    func finishNext(missingSecond: Bool = false) {
        let next = lock.withLock {
            precondition(pending != nil)
            let next = pending
            pending = nil
            return next
        }
        guard let (request, completion) = next else { preconditionFailure() }
        completion(.success(response(request, missingSecond: missingSecond)))
    }

    private func response(_ request: ReadRequest, missingSecond: Bool) -> ReadResult {
        let observations = (0..<2).map { index in
            Observation(run: "run-\(index)", boot: "boot", publication: 9, runTick: 9,
                        wallNs: (request.second ?? 17) * 1_000_000_000,
                        windowLosses: Losses(publications: 0, events: 0, unknown: false))
        }
        let observation = observations[Int(request.alternative)]
        let selection = Selection(second: request.second ?? 17, selected: observation,
                                  alternatives: observations, summary: nil, trends: [],
                                  contextSeconds: 900)
        let tables = ["ticks", "memory", "cpus", "interfaces", "cores", "states", "events", "processes"]
        let catalog = Catalog(path: request.path, firstSecond: 10, lastSecond: 17, windows: 1,
                              unknownClockTicks: 0, tables: tables)
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
