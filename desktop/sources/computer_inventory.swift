import Combine
import Darwin
import Foundation

struct ComputerInventorySnapshot: Equatable {
    let readAt: Date
    let modelIdentifier: ComputerInventoryReading<String>
    let chipName: ComputerInventoryReading<String>
    let macOSVersion: ComputerInventoryReading<String>
    let macOSBuild: ComputerInventoryReading<String>
    let kernelRelease: ComputerInventoryReading<String>
    let architecture: ComputerInventoryReading<String>
    let physicalCores: ComputerInventoryReading<UInt32>
    let logicalCores: ComputerInventoryReading<UInt32>
    let memoryBytes: ComputerInventoryReading<UInt64>
    let pageBytes: ComputerInventoryReading<UInt32>
    let bootTime: ComputerInventoryReading<Date>
}

struct ComputerInventoryReading<Value: Equatable>: Equatable {
    let key: String
    let result: Result<Value, ComputerInventoryFailure>

    var value: Value? {
        guard case .success(let value) = result else { return nil }

        return value
    }

    var failure: ComputerInventoryFailure? {
        guard case .failure(let failure) = result else { return nil }

        return failure
    }
}

enum ComputerInventoryFailure: Error, Equatable, CustomStringConvertible {
    case kernel(errno: Int32)
    case invalid(ComputerInventoryValidation)

    var description: String {
        switch self {
        case .kernel(let code): return "Kernel read failed (errno \(code))"
        case .invalid(let reason): return reason.description
        }
    }
}

enum ComputerInventoryValidation: Equatable, CustomStringConvertible {
    case size(expected: Int, actual: Int)
    case stringSize(actual: Int, capacity: Int)
    case unterminatedString
    case embeddedTerminator
    case invalidUTF8
    case nonpositiveValue
    case invalidBootTime

    var description: String {
        switch self {
        case .size(let expected, let actual):
            return "Invalid byte count: expected \(expected), received \(actual)"
        case .stringSize(let actual, let capacity):
            return "Invalid string byte count: received \(actual), capacity \(capacity)"
        case .unterminatedString: return "String has no final null terminator"
        case .embeddedTerminator: return "String contains an embedded null terminator"
        case .invalidUTF8: return "String contains invalid UTF-8"
        case .nonpositiveValue: return "Expected a positive value"
        case .invalidBootTime: return "Invalid boot timeval"
        }
    }
}

enum ComputerInventoryReadResult {
    case read(bytes: Int)
    case unavailable(errno: Int32)
}

typealias ComputerInventoryKernelRead = (String, UnsafeMutableRawBufferPointer)
    -> ComputerInventoryReadResult

struct ComputerInventoryReader {
    var read: ComputerInventoryKernelRead = readComputerInventoryKernel
    var now: () -> Date = Date.init

    func snapshot() -> ComputerInventorySnapshot {
        let readAt = now()

        return ComputerInventorySnapshot(readAt: readAt,
            modelIdentifier: string(key: "hw.product"),
            chipName: string(key: "machdep.cpu.brand_string"),
            macOSVersion: string(key: "kern.osproductversion"),
            macOSBuild: string(key: "kern.osversion"),
            kernelRelease: string(key: "kern.osrelease"),
            architecture: string(key: "hw.machine"),
            physicalCores: positiveCount(key: "hw.physicalcpu_max"),
            logicalCores: positiveCount(key: "hw.logicalcpu_max"),
            memoryBytes: memory(), pageBytes: positiveCount(key: "hw.pagesize"),
            bootTime: boot())
    }

    private func string(key: String) -> ComputerInventoryReading<String> {
        var bytes = [UInt8](repeating: 0, count: 512)
        let response = bytes.withUnsafeMutableBytes { read(key, $0) }
        guard case .read(let count) = response else { return unavailable(key, response) }

        guard count > 1, count <= bytes.count else {
            return invalid(key, .stringSize(actual: count, capacity: bytes.count))
        }

        guard bytes[count - 1] == 0 else { return invalid(key, .unterminatedString) }

        let content = bytes.prefix(count - 1)
        guard !content.contains(0) else { return invalid(key, .embeddedTerminator) }

        guard let value = String(bytes: content, encoding: .utf8) else {
            return invalid(key, .invalidUTF8)
        }

        return ComputerInventoryReading(key: key, result: .success(value))
    }

    private func positiveCount(key: String) -> ComputerInventoryReading<UInt32> {
        var value: Int32 = 0
        let response = withUnsafeMutableBytes(of: &value) { read(key, $0) }
        guard case .read(let count) = response else { return unavailable(key, response) }

        guard count == MemoryLayout<Int32>.size else {
            return invalid(key, .size(expected: MemoryLayout<Int32>.size, actual: count))
        }

        guard value > 0 else { return invalid(key, .nonpositiveValue) }

        return ComputerInventoryReading(key: key, result: .success(UInt32(value)))
    }

    private func memory() -> ComputerInventoryReading<UInt64> {
        let key = "hw.memsize"
        var value: UInt64 = 0
        let response = withUnsafeMutableBytes(of: &value) { read(key, $0) }
        guard case .read(let count) = response else { return unavailable(key, response) }

        guard count == MemoryLayout<UInt64>.size else {
            return invalid(key, .size(expected: MemoryLayout<UInt64>.size, actual: count))
        }

        guard value > 0 else { return invalid(key, .nonpositiveValue) }

        return ComputerInventoryReading(key: key, result: .success(value))
    }

    private func boot() -> ComputerInventoryReading<Date> {
        let key = "kern.boottime"
        var value = timeval()
        let response = withUnsafeMutableBytes(of: &value) { read(key, $0) }
        guard case .read(let count) = response else { return unavailable(key, response) }

        guard count == MemoryLayout<timeval>.size else {
            return invalid(key, .size(expected: MemoryLayout<timeval>.size, actual: count))
        }

        guard value.tv_sec > 0, value.tv_usec >= 0, value.tv_usec < 1_000_000 else {
            return invalid(key, .invalidBootTime)
        }

        let seconds = Double(value.tv_sec) + Double(value.tv_usec) / 1_000_000

        return ComputerInventoryReading(key: key,
            result: .success(Date(timeIntervalSince1970: seconds)))
    }

    private func unavailable<Value>(_ key: String, _ response: ComputerInventoryReadResult)
        -> ComputerInventoryReading<Value> {
        guard case .unavailable(let code) = response else {
            preconditionFailure("A completed kernel read must be decoded")
        }

        return ComputerInventoryReading(key: key, result: .failure(.kernel(errno: code)))
    }

    private func invalid<Value>(_ key: String, _ reason: ComputerInventoryValidation)
        -> ComputerInventoryReading<Value> {
        ComputerInventoryReading(key: key, result: .failure(.invalid(reason)))
    }
}

@MainActor
final class ComputerInventoryModel: ObservableObject {
    @Published private(set) var snapshot: ComputerInventorySnapshot?
    private let reader: ComputerInventoryReader

    init(reader: ComputerInventoryReader = ComputerInventoryReader()) {
        self.reader = reader
    }

    func loadIfNeeded() {
        guard snapshot == nil else { return }

        refresh()
    }

    func refresh() { snapshot = reader.snapshot() }
}

private func readComputerInventoryKernel(key: String, into buffer: UnsafeMutableRawBufferPointer)
    -> ComputerInventoryReadResult {
    var count = buffer.count
    return key.withCString { keyPointer in
        let status = sysctlbyname(keyPointer, buffer.baseAddress, &count, nil, 0)
        let code = errno
        guard status == 0 else { return .unavailable(errno: code) }

        return .read(bytes: count)
    }
}
