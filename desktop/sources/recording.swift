import AppKit
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
}

struct Observation: Decodable, Sendable {
    let run: String
    let boot: String
    let publication: UInt64
    let runTick: UInt64
    let wallNs: UInt64
    let windowLosses: Losses
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
}

struct ReadResult: Sendable {
    let catalog: Catalog
    let selection: Selection?
    let page: TablePage?
    let machinePages: [MachineSource: TablePage]
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
        if path != request.path || request.refresh {
            if let session { fd_close(session) }
            session = nil
            path = nil
            let bytes = Array(request.path.utf8)
            guard !bytes.isEmpty, bytes.count <= 4096 else {
                session = nil
                path = nil
                throw RecordingError(message: "Choose a recording directory with a valid path.")
            }
            session = bytes.withUnsafeBufferPointer {
                fd_open_controlled($0.baseAddress, $0.count, control, epoch)
            }
        }
        guard let session else { throw RecordingError(message: "Could not open the recording.") }
        try prepare(session, epoch: epoch)
        let catalog: Catalog = try decode(fd_catalog(session))
        path = request.path
        guard let second = request.second ?? catalog.lastSecond else {
            return ReadResult(catalog: catalog, selection: nil, page: nil, machinePages: [:])
        }
        try prepare(session, epoch: epoch)
        let selection: Selection = try decode(fd_select(session, second, request.alternative))
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
        return ReadResult(catalog: catalog, selection: selection, page: page,
                          machinePages: machinePages)
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
    @Published var catalog: Catalog?
    @Published var selection: Selection?
    @Published var page: TablePage?
    @Published var machinePages: [MachineSource: TablePage] = [:]
    @Published var error: String?
    @Published var busy = false
    @Published var route = "Overview"
    @Published var table: UInt32 = 7
    @Published var secondText = ""
    @Published var row: Int?
    private let reader: any RecordingReading
    private var generation: UInt64 = 0
    private var pending: (UInt64, ReadRequest)?
    private var requestedSecond: UInt64?
    private var alternative: UInt32 = 0
    private var requestedOffset: UInt64 = 0
    private var machineOffsets: [MachineSource: UInt64] = [:]

    init(path: String, reader: any RecordingReading = RecordingWorker()) {
        self.path = path
        self.reader = reader
    }

    func load(second: UInt64? = nil, alternative: UInt32? = nil, offset: UInt64 = 0,
              refresh: Bool = false) {
        if let second {
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

    private func enqueue(refresh: Bool) {
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
        let request = ReadRequest(path: path, second: requestedSecond, alternative: alternative,
            table: table, offset: requestedOffset, refresh: refresh || pending?.1.refresh == true,
            machineOffsets: machineOffsets, includeMachine: route == "CPU & processes")
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
                        selection = result.selection
                        page = result.page
                        machinePages = result.machinePages
                        requestedSecond = result.selection?.second
                        secondText = result.selection.map { String($0.second) } ?? ""
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
            path = url.path
            catalog = nil
            requestedSecond = nil
            alternative = 0
            machineOffsets.removeAll(keepingCapacity: true)
            selection = nil
            page = nil
            machinePages.removeAll(keepingCapacity: true)
            load(refresh: true)
        }
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
