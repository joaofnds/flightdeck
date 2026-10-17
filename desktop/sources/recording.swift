import AppKit
import Combine
import FlightdeckBridge
import Foundation
import SwiftUI

struct Catalog: Decodable, Sendable {
    let path: String
    let firstSecond: UInt64?
    let lastSecond: UInt64?
    let windows: UInt64
    let unknownClockTicks: UInt64
    let tables: [String]
    let live: Bool?
    let tailTicks: UInt64?
    let collectorCompleted: Bool?
    let latestObservation: Observation?
    let readerLosses: [ReaderLosses]?

    init(path: String, firstSecond: UInt64?, lastSecond: UInt64?, windows: UInt64,
         unknownClockTicks: UInt64, tables: [String], live: Bool? = nil,
         tailTicks: UInt64? = nil, collectorCompleted: Bool? = nil,
         latestObservation: Observation? = nil, readerLosses: [ReaderLosses]? = nil) {
        self.path = path
        self.firstSecond = firstSecond
        self.lastSecond = lastSecond
        self.windows = windows
        self.unknownClockTicks = unknownClockTicks
        self.tables = tables
        self.live = live
        self.tailTicks = tailTicks
        self.collectorCompleted = collectorCompleted
        self.latestObservation = latestObservation
        self.readerLosses = readerLosses
    }

}

struct Observation: Decodable, Sendable {
    let run: String
    let boot: String
    let publication: UInt64
    let runTick: UInt64
    let wallNs: UInt64
    let windowLosses: Losses?
    let readerLosses: Losses?

    init(run: String, boot: String, publication: UInt64, runTick: UInt64, wallNs: UInt64,
         windowLosses: Losses?, readerLosses: Losses? = nil) {
        self.run = run
        self.boot = boot
        self.publication = publication
        self.runTick = runTick
        self.wallNs = wallNs
        self.windowLosses = windowLosses
        self.readerLosses = readerLosses
    }
}

struct ReaderLosses: Decodable, Sendable {
    let run: String
    let boot: String
    let losses: Losses
}

struct Losses: Decodable, Sendable {
    let publications: UInt64
    let events: UInt64
    let unknown: Bool
    var hasLosses: Bool { publications > 0 || events > 0 || unknown }
}

struct Metric: Decodable, Identifiable, Sendable {
    let run: String
    let boot: String
    let wallNs: UInt64
    let publication: UInt64
    let segment: UInt64
    let cpuPercent: Double?
    let networkRxBps: Double?
    let networkTxBps: Double?
    let memoryTotalBytes: UInt64?
    let memoryActiveBytes: UInt64?
    let memoryWiredBytes: UInt64?
    let memoryCompressedBytes: UInt64?
    let swapUsedBytes: UInt64?
    let pressure: UInt64?
    let socCelsius: Double?
    let powerWatts: Double?
    let gpuWatts: Double?
    var id: String { "\(run):\(boot):\(publication)" }
    var date: Date { Date(timeIntervalSince1970: Double(wallNs) / 1e9) }
}

struct Selection: Decodable, Sendable {
    let second: UInt64
    let selected: Observation?
    let alternatives: [Observation]
    let summary: Metric?
    let trends: [Metric]
    let contextSeconds: UInt64
}

struct TablePage: Decodable, Sendable {
    let name: String
    let columns: [String]
    let rows: [[String?]]
    let offset: UInt64
    let total: UInt64
    let observation: Observation

    func value(_ row: [String?], _ column: String) -> String? {
        guard let index = columns.firstIndex(of: column), index < row.count else { return nil }
        return row[index]
    }
}

private struct Envelope<T: Decodable>: Decodable {
    let value: T?
    let error: String?
}

enum MachineSource: String, CaseIterable, Sendable {
    case ticks, memory, cpus, interfaces

    var title: String {
        switch self {
        case .ticks: "System readings"
        case .memory: "Memory & swap"
        case .cpus: "Logical CPU counters"
        case .interfaces: "Network interfaces"
        }
    }
}

struct ReadRequest: Sendable {
    let path: String
    let second: UInt64?
    let alternative: UInt32
    let table: UInt32
    let offset: UInt64
    let refresh: Bool
    let machineOffsets: [MachineSource: UInt64]
    let includeMachine: Bool
    let live: Bool
    let poll: Bool

    init(path: String, second: UInt64?, alternative: UInt32, table: UInt32, offset: UInt64,
         refresh: Bool, machineOffsets: [MachineSource: UInt64], includeMachine: Bool,
         live: Bool = false, poll: Bool = false) {
        self.path = path
        self.second = second
        self.alternative = alternative
        self.table = table
        self.offset = offset
        self.refresh = refresh
        self.machineOffsets = machineOffsets
        self.includeMachine = includeMachine
        self.live = live
        self.poll = poll
    }

}

struct ReadResult: Sendable {
    let catalog: Catalog
    let selection: Selection?
    let page: TablePage?
    let machinePages: [MachineSource: TablePage]
    let unchanged: Bool

    init(catalog: Catalog, selection: Selection?, page: TablePage?,
         machinePages: [MachineSource: TablePage], unchanged: Bool = false) {
        self.catalog = catalog
        self.selection = selection
        self.page = page
        self.machinePages = machinePages
        self.unchanged = unchanged
    }

}

struct RecordingError: Error, LocalizedError, Sendable {
    let message: String
    var errorDescription: String? { message }
}

protocol RecordingReading: Sendable {
    func cancel() throws
    func read(_ request: ReadRequest, completion: @escaping @Sendable
              (Result<ReadResult, RecordingError>) -> Void)
}

// Only queue work accesses the opaque Rust session. Main-thread state owns the mailbox.
final class RecordingWorker: RecordingReading, @unchecked Sendable {
    private let queue = DispatchQueue(label: "flightdeck.history", qos: .userInitiated)
    private let control = fd_control_new()
    private var session: OpaquePointer?
    private var path: String?
    private var openedLive = false
    private var lastResult: ReadResult?

    func read(_ request: ReadRequest, completion: @escaping @Sendable
              (Result<ReadResult, RecordingError>) -> Void) {
        guard let control else {
            completion(.failure(RecordingError(message: "Could not allocate query control.")))
            return
        }
        let epoch = fd_control_epoch(control)
        queue.async { [self] in
            do { completion(.success(try execute(request, epoch: epoch))) }
            catch { completion(.failure(RecordingError(message: error.localizedDescription))) }
        }
    }

    func cancel() throws {
        guard let control else {
            throw RecordingError(message: "Could not allocate query control.")
        }
        guard fd_control_cancel(control) == 0 else {
            throw RecordingError(message: "Could not cancel the running recording query.")
        }
    }

    private func execute(_ request: ReadRequest, epoch: UInt64) throws -> ReadResult {
        guard let control else {
            throw RecordingError(message: "Could not allocate query control.")
        }
        if path != request.path || request.refresh || openedLive != request.live {
            if let session { fd_close(session) }
            session = nil
            path = nil
            lastResult = nil
            let bytes = Array(request.path.utf8)
            guard !bytes.isEmpty, bytes.count <= 4096 else {
                session = nil
                path = nil
                throw RecordingError(message: "Choose a recording directory with a valid path.")
            }
            session = bytes.withUnsafeBufferPointer {
                if request.live {
                    fd_open_live_controlled($0.baseAddress, $0.count, control, epoch)
                } else {
                    fd_open_controlled($0.baseAddress, $0.count, control, epoch)
                }
            }
        }
        guard let session else { throw RecordingError(message: "Could not open the recording.") }
        try prepare(session, epoch: epoch)
        var changed = true
        if request.live, request.poll {
            changed = try decode(fd_live_poll(session))
        }
        let catalog: Catalog = try decode(fd_catalog(session))
        path = request.path
        openedLive = request.live
        if !changed, let lastResult {
            return ReadResult(catalog: catalog, selection: lastResult.selection,
                page: lastResult.page, machinePages: lastResult.machinePages, unchanged: true)
        }
        let latestSecond = catalog.latestObservation.map { $0.wallNs / 1_000_000_000 }
        guard let second = request.second ?? latestSecond ?? catalog.lastSecond else {
            return ReadResult(catalog: catalog, selection: nil, page: nil, machinePages: [:])
        }
        try prepare(session, epoch: epoch)
        var selection: Selection = try decode(fd_select(session, second, request.alternative))
        if request.live, request.second == nil, let latest = catalog.latestObservation,
           let index = selection.alternatives.firstIndex(where: {
               $0.run == latest.run && $0.boot == latest.boot && $0.publication == latest.publication
           }), index != Int(request.alternative) {
            selection = try decode(fd_select(session, second, UInt32(index)))
        }
        let page: TablePage?
        if selection.selected == nil {
            page = nil
        } else {
            try prepare(session, epoch: epoch)
            page = try decode(fd_page(session, request.table, request.offset))
        }
        var machinePages: [MachineSource: TablePage] = [:]
        if selection.selected != nil, request.includeMachine {
            for source in MachineSource.allCases {
                guard let index = catalog.tables.firstIndex(of: source.rawValue) else {
                    throw RecordingError(message: "The recording has no \(source.rawValue) source.")
                }
                let offset = request.machineOffsets[source] ?? 0
                if let page, page.name == source.rawValue, page.offset == offset {
                    machinePages[source] = page
                } else {
                    try prepare(session, epoch: epoch)
                    machinePages[source] = try decode(fd_page(session, UInt32(index), offset))
                }
            }
        }
        try prepare(session, epoch: epoch)
        let result = ReadResult(catalog: catalog, selection: selection, page: page,
                                machinePages: machinePages)
        lastResult = result
        return result
    }

    private func prepare(_ session: OpaquePointer, epoch: UInt64) throws {
        let ready: Bool = try decode(fd_prepare(session, epoch))
        guard ready else { throw RecordingError(message: "Recording query is not ready.") }
    }

    private func decode<T: Decodable>(_ bytes: FlightdeckBytes) throws -> T {
        defer { fd_bytes_free(bytes) }
        guard let pointer = bytes.pointer, bytes.length <= 8 * 1024 * 1024 else {
            throw RecordingError(message: "Invalid native query response.")
        }
        return try decodeRecordingResponse(Data(bytes: pointer, count: bytes.length))
    }

    deinit {
        if let session { fd_close(session) }
        if let control { fd_control_free(control) }
    }
}

@MainActor
final class RecordingModel: ObservableObject {
    @Published var path: String
    @Published var showsReference: Bool
    @Published var catalog: Catalog?
    @Published var selection: Selection?
    @Published var page: TablePage?
    @Published var machinePages: [MachineSource: TablePage] = [:]
    @Published var error: String?
    @Published var busy = false
    @Published var following = false
    @Published var route = "Overview"
    @Published var table: UInt32 = 7
    @Published var secondText = ""
    @Published var row: Int?
    private let reader: any RecordingReading
    private var useLive = false
    private let pollingEnabled: Bool
    private let now: @MainActor () -> Date
    private var liveTimer: AnyCancellable?
    private var generation: UInt64 = 0
    private var pending: (UInt64, ReadRequest)?
    private var requestedSecond: UInt64?
    private var alternative: UInt32 = 0
    private var requestedOffset: UInt64 = 0
    private var machineOffsets: [MachineSource: UInt64] = [:]

    init(path: String, reader: any RecordingReading = RecordingWorker(),
         pollingEnabled: Bool = true, now: @escaping @MainActor () -> Date = { Date() },
         showsReference: Bool = false) {
        self.path = path
        self.showsReference = showsReference
        self.reader = reader
        self.pollingEnabled = pollingEnabled
        self.now = now
    }

    var liveAvailable: Bool { catalog != nil }

    var readerLossNotices: [String] {
        var losses = catalog?.readerLosses ?? []
        if let selected = selection?.selected, let selectedLosses = selected.readerLosses,
           !losses.contains(where: { $0.run == selected.run && $0.boot == selected.boot }) {
            losses.append(ReaderLosses(run: selected.run, boot: selected.boot, losses: selectedLosses))
        }
        return losses.filter { $0.losses.hasLosses }.map { entry in
            "Live reader · run \(entry.run.prefix(8)): \(entry.losses.publications) missed publications, \(entry.losses.events) missed events"
                + (entry.losses.unknown ? "; additional loss is unknown." : ".")
        }
    }

    var liveStatus: String {
        guard useLive else { return "Recorded" }
        guard let catalog else { return "No live readings" }
        if catalog.collectorCompleted == true { return "Collector stopped" }
        let last = catalog.latestObservation.map { Double($0.wallNs) / 1e9 }
            ?? catalog.lastSecond.map(Double.init)
        guard let last else { return "No live readings" }
        let age = now().timeIntervalSince1970 - last
        if age < -3 { return "Clock changed" }
        if age > 3 { return "Waiting for collector" }
        return following ? "Live" : "Paused"
    }

    func setLive(_ enabled: Bool) {
        following = enabled
        liveTimer = nil
        guard enabled else { return }
        useLive = true
        requestedSecond = nil
        alternative = 0
        requestedOffset = 0
        machineOffsets.removeAll(keepingCapacity: true)
        enqueue(refresh: false)
        if pollingEnabled {
            liveTimer = Timer.publish(every: 0.25, on: .main, in: .common)
                .autoconnect().sink { [weak self] _ in self?.pollLive() }
        }
    }

    func pollLive() {
        guard following, !busy else { return }
        enqueue(refresh: false, poll: true)
    }

    func load(second: UInt64? = nil, alternative: UInt32? = nil, offset: UInt64 = 0,
              refresh: Bool = false) {
        if let second {
            if following { setLive(false) }
            if requestedSecond != second {
                self.alternative = 0
                machineOffsets.removeAll(keepingCapacity: true)
            }
            requestedSecond = second
        }
        if let alternative {
            if self.alternative != alternative { machineOffsets.removeAll(keepingCapacity: true) }
            self.alternative = alternative
        }
        requestedOffset = offset
        selection = nil
        page = nil
        machinePages.removeAll(keepingCapacity: true)
        row = nil
        enqueue(refresh: refresh)
    }

    private func enqueue(refresh: Bool, poll: Bool = false) {
        generation += 1
        error = nil
        if busy {
            do { try reader.cancel() }
            catch let failure {
                pending = nil
                error = failure.localizedDescription
                return
            }
        }
        let request = ReadRequest(path: path, second: following ? nil : requestedSecond,
            alternative: alternative, table: table, offset: requestedOffset,
            refresh: refresh || pending?.1.refresh == true,
            machineOffsets: machineOffsets, includeMachine: route == "CPU & processes",
            live: useLive, poll: poll)
        if busy { pending = (generation, request) }
        else { start(generation, request) }
    }

    private func start(_ token: UInt64, _ request: ReadRequest) {
        busy = true
        reader.read(request) { [weak self] result in
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                busy = false
                if token == generation {
                    switch result {
                    case .success(let result):
                        catalog = result.catalog
                        if !result.unchanged, !request.poll || following {
                            selection = result.selection
                            page = result.page
                            machinePages = result.machinePages
                            requestedSecond = result.selection?.second
                            secondText = result.selection.map { String($0.second) } ?? ""
                        }
                    case .failure(let failure):
                        error = failure.message
                    }
                }
                if let (nextToken, nextRequest) = pending {
                    pending = nil
                    start(nextToken, nextRequest)
                }
            }
        }
    }

    func cancel() {
        setLive(false)
        generation += 1
        pending = nil
        if busy {
            do { try reader.cancel() }
            catch { self.error = error.localizedDescription }
        }
    }

    func open() {
        let picker = NSOpenPanel()
        picker.canChooseDirectories = true
        picker.canChooseFiles = false
        picker.allowsMultipleSelection = false
        picker.message = "Choose a Flightdeck recording directory."
        if picker.runModal() == .OK, let url = picker.url {
            chooseRecording(path: url.path)
        }
    }

    func chooseRecording(path: String) {
        setLive(false)
        useLive = false
        showsReference = false
        self.path = path
        catalog = nil
        requestedSecond = nil
        alternative = 0
        machineOffsets.removeAll(keepingCapacity: true)
        selection = nil
        page = nil
        machinePages.removeAll(keepingCapacity: true)
        load(refresh: true)
    }

    func chooseTable(_ index: UInt32) {
        let second = selection?.second
        table = index
        load(second: second)
    }

    func enterProcesses() {
        route = "CPU & processes"
        chooseTable(7)
    }

    func pageMachine(_ source: MachineSource, offset: UInt64) {
        precondition(offset.isMultiple(of: 512))
        machineOffsets[source] = offset
        enqueue(refresh: false)
    }
}

func decodeRecordingResponse<T: Decodable>(_ data: Data) throws -> T {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let envelope = try decoder.decode(Envelope<T>.self, from: data)
    if let error = envelope.error { throw RecordingError(message: error) }
    guard let value = envelope.value else {
        throw RecordingError(message: "The recording query returned no value.")
    }
    return value
}
