import AppKit
import Combine
import FlightdeckBridge
import Foundation
import SwiftUI

struct Catalog: Decodable, Equatable, Sendable {
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
    let latestCollectorPid: UInt32?
    let readerLosses: [ReaderLosses]?
    let historyError: String?

    init(path: String, firstSecond: UInt64?, lastSecond: UInt64?, windows: UInt64,
         unknownClockTicks: UInt64, tables: [String], live: Bool? = nil,
         tailTicks: UInt64? = nil, collectorCompleted: Bool? = nil,
         latestObservation: Observation? = nil, readerLosses: [ReaderLosses]? = nil,
         latestCollectorPid: UInt32? = nil, historyError: String? = nil) {
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
        self.latestCollectorPid = latestCollectorPid
        self.readerLosses = readerLosses
        self.historyError = historyError
    }

}

struct Observation: Decodable, Equatable, Sendable {
    let run: String
    let boot: String
    let publication: UInt64
    let runTick: UInt64
    let wallNs: UInt64
    let windowLosses: Losses?
    let readerLosses: Losses?
    var identity: RecordedObservationIdentity {
        RecordedObservationIdentity(run: run, boot: boot, publication: publication)
    }

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

struct ReaderLosses: Decodable, Equatable, Sendable {
    let run: String
    let boot: String
    let losses: Losses
}

struct Losses: Decodable, Equatable, Sendable {
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
    let runTick: UInt64?
    let segment: UInt64
    let cpuPercent: Double?
    let gpuActivePercent: Double?
    let networkRxBps: Double?
    let networkTxBps: Double?
    let networkRxPacketsPerSecond: Double?
    let networkTxPacketsPerSecond: Double?
    let networkRxErrorsPerSecond: Double?
    let networkTxErrorsPerSecond: Double?
    let memoryTotalBytes: UInt64?
    let memoryFreeBytes: UInt64?
    let memoryActiveBytes: UInt64?
    let memoryInactiveBytes: UInt64?
    let memorySpeculativeBytes: UInt64?
    let memoryWiredBytes: UInt64?
    let memoryCompressedBytes: UInt64?
    let memoryAnonymousBytes: UInt64?
    let memoryFileBackedBytes: UInt64?
    let memoryPurgeableBytes: UInt64?
    let swapUsedBytes: UInt64?
    let swapTotalBytes: UInt64?
    let swapInBytesPerSecond: Double?
    let swapOutBytesPerSecond: Double?
    let memoryPageinsPerSecond: Double?
    let memoryCompressionsPerSecond: Double?
    let memoryDecompressionsPerSecond: Double?
    let pressure: UInt64?
    let socCelsius: Double?
    let fanRpm: FanRPMReadings?
    let powerWatts: Double?
    let gpuWatts: Double?
    let battery: RecordedBatteryReading?
    let storage: RecordedStorageReading?
    var identity: RecordedObservationIdentity {
        RecordedObservationIdentity(run: run, boot: boot, publication: publication)
    }
    var id: String { "\(run):\(boot):\(publication)" }
    var date: Date { Date(timeIntervalSince1970: Double(wallNs) / 1e9) }
}

struct RecordedObservationIdentity: Hashable, Sendable {
    let run: String
    let boot: String
    let publication: UInt64
}

struct Selection: Decodable, Sendable {
    let second: UInt64
    let selected: Observation?
    let alternatives: [Observation]
    let summary: Metric?
    let trends: [Metric]
    let contextSeconds: UInt64
    let contextError: String?

    init(second: UInt64, selected: Observation?, alternatives: [Observation], summary: Metric?,
         trends: [Metric], contextSeconds: UInt64, contextError: String? = nil) {
        self.second = second
        self.selected = selected
        self.alternatives = alternatives
        self.summary = summary
        self.trends = trends
        self.contextSeconds = contextSeconds
        self.contextError = contextError
    }
}

struct SelectionHeader: Decodable {
    let second: UInt64
    let selected: Observation?
    let alternatives: [Observation]
    let contextSeconds: UInt64
    let contextError: String?
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
    let value: T
    private enum CodingKeys: String, CodingKey { case value, error }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if let error = try container.decodeIfPresent(String.self, forKey: .error) {
            throw RecordingError(message: error)
        }

        value = try container.decode(T.self, forKey: .value)
    }
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
    let includePage: Bool
    let includeFans: Bool
    let includeCPUDetail: Bool
    let cpuHistoryOffset: UInt32?
    let catalogOnly: Bool
    let live: Bool
    let poll: Bool
    let target: RecordedObservationIdentity?
    let apps: LiveAppsRequest?

    init(path: String, second: UInt64?, alternative: UInt32, table: UInt32, offset: UInt64,
         refresh: Bool, machineOffsets: [MachineSource: UInt64], includeMachine: Bool,
         includePage: Bool = true, includeFans: Bool = false,
         live: Bool = false, poll: Bool = false, target: RecordedObservationIdentity? = nil,
         apps: LiveAppsRequest? = nil, catalogOnly: Bool = false, includeCPUDetail: Bool = false,
         cpuHistoryOffset: UInt32? = nil) {
        self.path = path
        self.second = second
        self.alternative = alternative
        self.table = table
        self.offset = offset
        self.refresh = refresh
        self.machineOffsets = machineOffsets
        self.includeMachine = includeMachine
        self.includePage = includePage
        self.includeFans = includeFans
        self.includeCPUDetail = includeCPUDetail
        self.cpuHistoryOffset = cpuHistoryOffset
        self.catalogOnly = catalogOnly
        self.live = live
        self.poll = poll
        self.target = target
        self.apps = apps
    }

    func hasSameProjection(as other: ReadRequest) -> Bool {
        path == other.path && live == other.live && second == other.second
            && alternative == other.alternative && target == other.target
            && table == other.table && offset == other.offset
            && machineOffsets == other.machineOffsets && includeMachine == other.includeMachine
            && includePage == other.includePage && includeFans == other.includeFans && apps == other.apps
            && catalogOnly == other.catalogOnly
            && includeCPUDetail == other.includeCPUDetail
            && cpuHistoryOffset == other.cpuHistoryOffset
    }
}

struct ReadResult: Sendable {
    let catalog: Catalog
    let selection: Selection?
    let page: TablePage?
    let machinePages: [MachineSource: TablePage]
    let unchanged: Bool
    let apps: LiveAppsRead?
    let fans: SelectedFanReadings?
    let cpuDetail: SelectedCPUReadings?
    let cpuHistory: CPUHistoryRead?

    init(catalog: Catalog, selection: Selection?, page: TablePage?,
         machinePages: [MachineSource: TablePage], unchanged: Bool = false, apps: LiveAppsRead? = nil,
         fans: SelectedFanReadings? = nil, cpuDetail: SelectedCPUReadings? = nil,
         cpuHistory: CPUHistoryRead? = nil) {
        self.catalog = catalog
        self.selection = selection
        self.page = page
        self.machinePages = machinePages
        self.unchanged = unchanged
        self.apps = apps
        self.fans = fans
        self.cpuDetail = cpuDetail
        self.cpuHistory = cpuHistory
    }

}

enum CPUHistoryRead: Sendable {
    case readings(SelectedCPUHistory)
    case unavailable(String)

    var readings: SelectedCPUHistory? {
        if case .readings(let value) = self { return value }

        return nil
    }

    var notice: String? {
        if case .unavailable(let message) = self { return message }
        if case .readings(let value) = self { return value.restorationError }

        return nil
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
    private var lastRequest: ReadRequest?

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
            lastRequest = nil
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
        if request.catalogOnly {
            lastResult = nil
            lastRequest = nil
            return ReadResult(catalog: catalog, selection: nil, page: nil,
                machinePages: [:], unchanged: true)
        }
        if !changed, let lastResult, lastRequest?.hasSameProjection(as: request) == true,
           lastResult.catalog.windows == catalog.windows,
           lastResult.catalog.historyError == catalog.historyError,
           lastResult.cpuHistory?.notice == nil,
           lastResult.apps?.history?.restorationError == nil,
           request.target == nil || lastResult.selection?.selected?.identity == request.target {
            return ReadResult(catalog: catalog, selection: lastResult.selection,
                page: lastResult.page, machinePages: lastResult.machinePages, unchanged: true,
                apps: lastResult.apps, fans: lastResult.fans, cpuDetail: lastResult.cpuDetail,
                cpuHistory: lastResult.cpuHistory)
        }
        let latestSecond = catalog.latestObservation.map { $0.wallNs / 1_000_000_000 }
        guard let second = request.second ?? latestSecond ?? catalog.lastSecond else {
            lastResult = nil
            lastRequest = nil
            return ReadResult(catalog: catalog, selection: nil, page: nil, machinePages: [:])
        }
        try prepare(session, epoch: epoch)
        let target = request.target
        var selection: Selection
        if request.live, request.second == nil, target == nil {
            selection = try decodeSelection(fd_follow(session), includeMetrics: request.apps == nil)
        } else {
            selection = try decodeSelection(fd_select(session, second,
                target == nil ? request.alternative : 0), includeMetrics: request.apps == nil)
        }
        if let target {
            let alternative = try recordedAlternative(selection: selection, target: target)
            if selection.selected?.identity != target {
                try prepare(session, epoch: epoch)
                selection = try decodeSelection(fd_select(session, second, alternative),
                    includeMetrics: request.apps == nil)
            }
            guard selection.selected?.identity == target else {
                throw RecordingError(message: "The requested observation is no longer available.")
            }
        }
        let page: TablePage?
        if selection.selected == nil || !request.includePage || request.apps != nil {
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
        let apps = try readApps(request.apps, session: session, selected: selection.selected, epoch: epoch)
        let fans = try readFans(request.includeFans, session: session, catalog: catalog,
            selected: selection.selected, epoch: epoch)
        let cpuDetail = try readCPUDetail(request.includeCPUDetail, session: session,
            selected: selection.selected, epoch: epoch)
        let cpuHistory = try readCPUHistory(request.cpuHistoryOffset, session: session,
            selection: selection, cpuDetail: cpuDetail, epoch: epoch)
        try prepare(session, epoch: epoch)
        let result = ReadResult(catalog: catalog, selection: selection, page: page,
            machinePages: machinePages, apps: apps, fans: fans, cpuDetail: cpuDetail,
            cpuHistory: cpuHistory)
        lastResult = result
        lastRequest = request
        return result
    }

    private func prepare(_ session: OpaquePointer, epoch: UInt64) throws {
        let ready: Bool = try decode(fd_prepare(session, epoch))
        guard ready else { throw RecordingError(message: "Recording query is not ready.") }
    }

    private func readFans(_ requested: Bool, session: OpaquePointer, catalog: Catalog,
                          selected: Observation?, epoch: UInt64) throws -> SelectedFanReadings? {
        guard requested, let selected else { return nil }

        guard let index = catalog.tables.firstIndex(of: "fans"), let table = UInt32(exactly: index) else {
            throw RecordingError(message: "The recording has no fans source.")
        }

        try prepare(session, epoch: epoch)
        let page: TablePage = try decode(fd_page(session, table, 0))

        return try SelectedFanReadings(page: page, selected: selected)
    }

    private func readCPUDetail(_ requested: Bool, session: OpaquePointer, selected: Observation?,
                               epoch: UInt64) throws -> SelectedCPUReadings? {
        guard requested, let selected else { return nil }

        try prepare(session, epoch: epoch)
        let payload: CPUDetailPayload = try decode(fd_cpu_detail(session))

        return try SelectedCPUReadings(payload: payload, selected: selected)
    }

    private func readCPUHistory(_ requestedOffset: UInt32?, session: OpaquePointer,
        selection: Selection, cpuDetail: SelectedCPUReadings?, epoch: UInt64) throws -> CPUHistoryRead? {
        guard let requestedOffset, selection.selected != nil else { return nil }

        do {
            guard requestedOffset.isMultiple(of: SelectedCPUHistory.cpusPerPage) else {
                throw CPUHistoryError.page
            }

            var offset = requestedOffset
            if let cpuDetail, offset >= cpuDetail.cpus.count { offset = 0 }

            try prepare(session, epoch: epoch)
            let payload: CPUHistoryPayload = try decode(fd_cpu_history(session, offset))
            let history = try SelectedCPUHistory(payload: payload, selection: selection)

            return .readings(history)
        } catch {
            // A canceled epoch must reject the whole result instead of installing stale readings.
            try prepare(session, epoch: epoch)

            return .unavailable(String(error.localizedDescription.prefix(512)))
        }
    }

    private func readApps(_ request: LiveAppsRequest?, session: OpaquePointer,
                          selected: Observation?, epoch: UInt64) throws -> LiveAppsRead? {
        guard let request, let selected else { return nil }

        let list = try readAppsList(request, session: session, epoch: epoch)
        guard list.header.selected.matches(selected) else {
            throw RecordingError(message: "App totals do not match the selected observation.")
        }

        let key = request.key ?? list.apps.first?.key.identity
        guard let key else {
            return LiveAppsRead(key: nil, list: list, detail: nil, history: nil, notice: nil)
        }

        try prepare(session, epoch: epoch)
        let history: LiveAppHistoryPayload = try key.bytes.withUnsafeBytes { bytes in
            var input = FlightdeckAppKey(kind: key.kind == .bundle ? 0 : 1,
                pointer: bytes.bindMemory(to: UInt8.self).baseAddress, length: bytes.count)

            return try decode(fd_app_history(session, &input))
        }
        guard history.selected == list.header.selected else {
            throw RecordingError(message: "App history does not match the selected observation.")
        }
        let detail = try readAppsDetail(request, key: key, session: session, epoch: epoch)
        if let detail {
            guard detail.header.selected == list.header.selected, detail.app.key.identity == key else {
                throw RecordingError(message: "App processes do not match the selected observation.")
            }
        }

        return LiveAppsRead(key: key, list: list, detail: detail, history: history,
            notice: detail == nil ? "No processes for the selected App at this reading." : nil)
    }

    private func readAppsList(_ request: LiveAppsRequest, session: OpaquePointer,
                              epoch: UInt64) throws -> LiveAppListPage {
        try prepare(session, epoch: epoch)
        let first: LiveAppListPage = try decode(fd_apps(session, 0))
        let offset = LiveAppsRequest.validOffset(requested: request.appsOffset, total: first.page.total)
        guard offset > 0 else { return first }

        try prepare(session, epoch: epoch)

        return try decode(fd_apps(session, offset))
    }

    private func readAppsDetail(_ request: LiveAppsRequest, key: LiveAppKeyIdentity,
                                session: OpaquePointer, epoch: UInt64) throws -> LiveAppDetailPage? {
        let first = try readAppsDetailPage(key, offset: 0, session: session, epoch: epoch)
        guard let first else { return nil }

        let offset = LiveAppsRequest.validOffset(requested: request.membersOffset, total: first.page.total)
        guard offset > 0 else { return first }

        return try readAppsDetailPage(key, offset: offset, session: session, epoch: epoch)
    }

    private func readAppsDetailPage(_ key: LiveAppKeyIdentity, offset: UInt32,
                                    session: OpaquePointer, epoch: UInt64) throws -> LiveAppDetailPage? {
        try prepare(session, epoch: epoch)

        return try key.bytes.withUnsafeBytes { bytes in
            var input = FlightdeckAppKey(kind: key.kind == .bundle ? 0 : 1,
                pointer: bytes.bindMemory(to: UInt8.self).baseAddress, length: bytes.count)

            return try decode(fd_app_detail(session, &input, offset))
        }
    }

    private func decodeSelection(_ bytes: FlightdeckBytes, includeMetrics: Bool) throws -> Selection {
        if includeMetrics { return try decode(bytes) }

        let header: SelectionHeader = try decode(bytes)

        return Selection(second: header.second, selected: header.selected,
            alternatives: header.alternatives, summary: nil, trends: [],
            contextSeconds: header.contextSeconds, contextError: header.contextError)
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
    @Published private(set) var apps: LiveAppsRead?
    @Published private(set) var appKey: LiveAppKeyIdentity?
    @Published private(set) var appProcessGroup: Data?
    @Published private(set) var fans: SelectedFanReadings?
    @Published private(set) var cpuDetail: SelectedCPUReadings?
    @Published private(set) var cpuHistory: CPUHistoryRead?
    private let reader: any RecordingReading
    private var useLive = false
    private var startedLive = false
    private let pollingEnabled: Bool
    private let now: @MainActor () -> Date
    private var liveTimer: AnyCancellable?
    private var reading = false
    private var announcedLiveStatus = "Recorded"
    private var generation: UInt64 = 0
    private var pending: (UInt64, ReadRequest)?
    private var requestedSecond: UInt64?
    private var alternative: UInt32 = 0
    private var requestedIdentity: RecordedObservationIdentity?
    private var requestedOffset: UInt64 = 0
    private var machineOffsets: [MachineSource: UInt64] = [:]
    private var appsOffset: UInt32 = 0
    private var appMembersOffset: UInt32 = 0
    private var cpuHistoryOffset: UInt32 = 0

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
    var isLiveSource: Bool { useLive }
    private var readsCatalogOnly: Bool { route == "Computer" || route == "Self-monitoring" }

    func startLive() {
        guard !startedLive else { return }

        startedLive = true
        setLive(true)
    }

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
        if !following { return "Paused" }
        let last = catalog.latestObservation.map { Double($0.wallNs) / 1e9 }
            ?? catalog.lastSecond.map { Double($0) }
        guard let last else { return "No live readings" }
        let age = now().timeIntervalSince1970 - last
        if age < -3 { return "Clock changed" }
        if age > 3 { return "Waiting for collector" }
        return "Live"
    }

    func setLive(_ enabled: Bool) {
        guard enabled else {
            stopFollowing()
            if reading {
                if requestedIdentity != nil { enqueue(refresh: false) }
                else { cancel() }
            }

            return
        }

        following = true
        liveTimer = nil
        useLive = true
        requestedSecond = nil
        requestedIdentity = nil
        alternative = 0
        requestedOffset = 0
        machineOffsets.removeAll(keepingCapacity: true)
        appsOffset = 0
        appMembersOffset = 0
        cpuHistoryOffset = 0
        enqueue(refresh: false)
        if pollingEnabled {
            liveTimer = Timer.publish(every: 0.25, on: .main, in: .common)
                .autoconnect().sink { [weak self] _ in self?.pollLive() }
        }
    }

    private func stopFollowing() {
        following = false
        liveTimer = nil

        if let selected = selection?.selected {
            requestedSecond = selection?.second
            requestedIdentity = selected.identity
        }
    }

    func pollLive() {
        guard following, !reading else { return }

        enqueue(refresh: false, poll: true)
    }

    func refreshDiagnostics() { enqueue(refresh: true) }

    func load(second: UInt64? = nil, alternative: UInt32? = nil, offset: UInt64 = 0,
              refresh: Bool = false) {
        if let second {
            if following { stopFollowing() }
            appsOffset = 0
            appMembersOffset = 0
            requestedIdentity = nil
            if requestedSecond != second {
                self.alternative = 0
                machineOffsets.removeAll(keepingCapacity: true)
            }
            requestedSecond = second
        }
        if let alternative {
            requestedIdentity = nil
            if self.alternative != alternative { machineOffsets.removeAll(keepingCapacity: true) }
            self.alternative = alternative
        }
        requestedOffset = offset
        cpuHistoryOffset = 0
        selection = nil
        fans = nil
        cpuDetail = nil
        cpuHistory = nil
        apps = nil
        page = nil
        machinePages.removeAll(keepingCapacity: true)
        row = nil
        enqueue(refresh: refresh)
    }

    private func enqueue(refresh: Bool, poll: Bool = false) {
        generation += 1
        if !poll, error != nil { error = nil }
        if !poll, !busy { busy = true }
        if reading {
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
            includePage: !useLive || route == "CPU & processes" || route == "Recorded readings",
            includeFans: route == "Overview" && !showsReference,
            live: useLive, poll: poll, target: following ? nil : requestedIdentity,
            apps: route == "Apps" ? LiveAppsRequest(key: appKey, appsOffset: appsOffset,
                membersOffset: appMembersOffset) : nil, catalogOnly: readsCatalogOnly,
            includeCPUDetail: route == "CPU & processes" && !showsReference,
            cpuHistoryOffset: route == "CPU & processes" && !showsReference ? cpuHistoryOffset : nil)
        if reading { pending = (generation, request) }
        else { start(generation, request) }
    }

    private func start(_ token: UInt64, _ request: ReadRequest) {
        reading = true
        if !request.poll, !busy { busy = true }
        reader.read(request) { [weak self] result in
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                reading = false
                if busy { busy = false }
                if token == generation {
                    switch result {
                    case .success(let result):
                        if error != nil { error = nil }
                        if catalog != result.catalog { catalog = result.catalog }
                        if !result.unchanged, !request.poll || following {
                            selection = result.selection
                            fans = result.fans
                            cpuDetail = result.cpuDetail
                            cpuHistory = result.cpuHistory
                            if let history = result.cpuHistory?.readings {
                                cpuHistoryOffset = history.offset
                            }
                            page = result.page
                            machinePages = result.machinePages
                            apps = result.apps
                            if let reading = result.apps {
                                appsOffset = reading.list.page.offset
                                appMembersOffset = reading.detail?.page.offset ?? 0
                                if let key = reading.key, appKey != key { appKey = key }
                            }
                            requestedSecond = result.selection?.second
                            if !following {
                                requestedIdentity = result.selection?.selected?.identity
                            }
                            secondText = result.selection.map { String($0.second) } ?? ""
                        }
                    case .failure(let failure):
                        if error != failure.message { error = failure.message }
                    }
                    publishStatusChange()
                }
                if let (nextToken, nextRequest) = pending {
                    pending = nil
                    start(nextToken, nextRequest)
                }
            }
        }
    }

    private func publishStatusChange() {
        let status = liveStatus
        if status != announcedLiveStatus {
            announcedLiveStatus = status
            objectWillChange.send()
        }
    }

    func cancel() {
        stopFollowing()
        generation += 1
        pending = nil
        if reading {
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
        stopFollowing()
        useLive = false
        showsReference = false
        if route == "Apps" { route = "Overview" }
        self.path = path
        catalog = nil
        requestedSecond = nil
        requestedIdentity = nil
        alternative = 0
        machineOffsets.removeAll(keepingCapacity: true)
        selection = nil
        fans = nil
        cpuDetail = nil
        cpuHistory = nil
        apps = nil
        appKey = nil
        appProcessGroup = nil
        appsOffset = 0
        appMembersOffset = 0
        page = nil
        machinePages.removeAll(keepingCapacity: true)
        load(refresh: true)
    }

    func chooseTable(_ index: UInt32) {
        table = index
        load()
    }

    func enterProcesses() {
        route = "CPU & processes"
        table = 7
        requestedOffset = 0
        if !following, let selected = selection?.selected {
            requestedSecond = selection?.second
            requestedIdentity = selected.identity
        }
        page = nil
        machinePages.removeAll(keepingCapacity: true)
        row = nil
        enqueue(refresh: false)
    }

    func enterRawReadings() {
        route = "Recorded readings"
        enqueue(refresh: false)
    }

    func enterOverview() {
        route = "Overview"
        enqueue(refresh: false)
    }

    func enterMetrics(_ title: String) {
        let needsMetrics = route == "Apps" || readsCatalogOnly
        route = title

        if needsMetrics { enqueue(refresh: false) }
    }

    func navigate(to title: String) {
        if let component = RecordedComponent(rawValue: title) {
            enterComponent(component)
        } else if title == "Memory" {
            enterMemory()
        } else if title == "Battery" || title == "Storage" {
            enterMetrics(title)
        } else if title == "Apps" { enterApps() }
        else if title == "Overview" { enterOverview() }
        else if title == "Recorded readings" { enterRawReadings() }
        else { route = title }
    }

    func enterApps() {
        route = "Apps"
        enqueue(refresh: false)
    }

    func selectApp(_ key: LiveAppKeyIdentity) {
        appKey = key
        appProcessGroup = nil
        appMembersOffset = 0
        apps = nil
        enqueue(refresh: false)
    }

    func selectAppProcessGroup(_ key: Data?) {
        appProcessGroup = key
    }

    func pageApps(_ offset: UInt32) {
        appsOffset = offset
        enqueue(refresh: false)
    }

    func pageAppMembers(_ offset: UInt32) {
        appMembersOffset = offset
        enqueue(refresh: false)
    }

    func select(_ target: RecordedSelectionTarget) {
        if following { stopFollowing() }
        requestedSecond = target.second
        requestedIdentity = target.identity
        appsOffset = 0
        appMembersOffset = 0
        cpuHistoryOffset = 0
        alternative = 0
        requestedOffset = 0
        machineOffsets.removeAll(keepingCapacity: true)
        selection = nil
        fans = nil
        cpuDetail = nil
        cpuHistory = nil
        page = nil
        machinePages.removeAll(keepingCapacity: true)
        row = nil
        apps = nil
        enqueue(refresh: false)
    }

    func pageCPUHistory(_ offset: UInt32) {
        precondition(offset.isMultiple(of: SelectedCPUHistory.cpusPerPage))
        precondition(offset < UInt32(SelectedCPUReadings.cpusMax))
        cpuHistoryOffset = offset
        enqueue(refresh: false)
    }

    func pageMachine(_ source: MachineSource, offset: UInt64) {
        precondition(offset.isMultiple(of: 512))
        machineOffsets[source] = offset
        enqueue(refresh: false)
    }
}

func recordedAlternative(selection: Selection, target: RecordedObservationIdentity) throws -> UInt32 {
    guard let index = selection.alternatives.firstIndex(where: { $0.identity == target }),
          let alternative = UInt32(exactly: index) else {
        throw RecordingError(message: "The requested observation is no longer available.")
    }

    return alternative
}

func decodeRecordingResponse<T: Decodable>(_ data: Data) throws -> T {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try decoder.decode(Envelope<T>.self, from: data).value
}
