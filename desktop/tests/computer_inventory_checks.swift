import Darwin
import Foundation

@MainActor
func runComputerInventoryChecks() {
    preserves_current_host_inventory_without_marketing_guesses()
    bounds_inventory_to_fixed_keys_and_one_read_each()
    preserves_independent_kernel_failure()
    preserves_a_full_capacity_inventory_string()
    preserves_whole_second_boot_time()
    rejects_inventory_string(bytes: [], count: 0, reason: .stringSize(actual: 0, capacity: 512))
    rejects_inventory_string(bytes: [0], count: 1, reason: .stringSize(actual: 1, capacity: 512))
    rejects_inventory_string(bytes: [65, 0], count: 513,
        reason: .stringSize(actual: 513, capacity: 512))
    rejects_inventory_string(bytes: [65, 66], count: 2, reason: .unterminatedString)
    rejects_inventory_string(bytes: [65, 0, 66, 0], count: 4, reason: .embeddedTerminator)
    rejects_inventory_string(bytes: [0xC3, 0x28, 0], count: 3, reason: .invalidUTF8)
    rejects_inventory_count(bytes: inventoryBytes(Int32(1)), count: 3,
        reason: .size(expected: 4, actual: 3))
    rejects_inventory_count(bytes: inventoryBytes(Int32(1)), count: 8,
        reason: .size(expected: 4, actual: 8))
    rejects_inventory_count(bytes: inventoryBytes(Int32(0)), count: 4, reason: .nonpositiveValue)
    rejects_inventory_count(bytes: inventoryBytes(Int32(-1)), count: 4, reason: .nonpositiveValue)
    rejects_inventory_memory(bytes: inventoryBytes(UInt64(1)), count: 4,
        reason: .size(expected: 8, actual: 4))
    rejects_inventory_memory(bytes: inventoryBytes(UInt64(0)), count: 8, reason: .nonpositiveValue)
    rejects_inventory_boot(value: timeval(tv_sec: 1, tv_usec: 0), count: 0,
        reason: .size(expected: MemoryLayout<timeval>.size, actual: 0))
    rejects_inventory_boot(value: timeval(tv_sec: 0, tv_usec: 0),
        count: MemoryLayout<timeval>.size, reason: .invalidBootTime)
    rejects_inventory_boot(value: timeval(tv_sec: -1, tv_usec: 0),
        count: MemoryLayout<timeval>.size, reason: .invalidBootTime)
    rejects_inventory_boot(value: timeval(tv_sec: 1, tv_usec: -1),
        count: MemoryLayout<timeval>.size, reason: .invalidBootTime)
    rejects_inventory_boot(value: timeval(tv_sec: 1, tv_usec: 1_000_000),
        count: MemoryLayout<timeval>.size, reason: .invalidBootTime)
    reads_inventory_once_until_explicit_refresh()
    replaces_successful_inventory_with_refresh_failures()
    keeps_initial_failure_until_explicit_refresh_recovers()
    print("Current computer inventory checks passed")
}

private func preserves_current_host_inventory_without_marketing_guesses() {
    // Goal: all current-host facts preserve their independent source values and read time.
    // Method: return fixed native bytes at the kernel boundary and compare the typed snapshot.
    let kernel = ComputerInventoryKernelFake()
    let instant = Date(timeIntervalSince1970: 1_800_000_000)
    let reader = ComputerInventoryReader(read: kernel.read, now: { instant })

    let snapshot = reader.snapshot()

    precondition(snapshot.readAt == instant)
    precondition(snapshot.modelIdentifier.value == "Mac17,8")
    precondition(snapshot.modelIdentifier.key == "hw.product")
    precondition(snapshot.chipName.value == "Apple M5 Pro")
    precondition(snapshot.macOSVersion.value == "26.2")
    precondition(snapshot.macOSBuild.value == "25C99")
    precondition(snapshot.kernelRelease.value == "25.2.0")
    precondition(snapshot.architecture.value == "arm64")
    precondition(snapshot.physicalCores.value == 18)
    precondition(snapshot.logicalCores.value == 18)
    precondition(snapshot.memoryBytes.value == 51_539_607_552)
    precondition(snapshot.pageBytes.value == 16_384)
    precondition(snapshot.bootTime.value == Date(timeIntervalSince1970: 1_700_000_000.25))
}

private func bounds_inventory_to_fixed_keys_and_one_read_each() {
    // Goal: a snapshot performs exactly eleven bounded reads, with no size queries or retries.
    // Method: inspect the outbound native-buffer contract at the injected kernel boundary.
    let kernel = ComputerInventoryKernelFake()

    let snapshot = ComputerInventoryReader(read: kernel.read).snapshot()

    precondition(snapshot.modelIdentifier.failure == nil)
    precondition(kernel.requests.count == 11)
    precondition(Dictionary(uniqueKeysWithValues: kernel.requests.map { ($0.key, $0.capacity) })
        == ["hw.product": 512, "machdep.cpu.brand_string": 512,
            "kern.osproductversion": 512, "kern.osversion": 512, "kern.osrelease": 512,
            "hw.machine": 512, "hw.physicalcpu_max": MemoryLayout<Int32>.size,
            "hw.logicalcpu_max": MemoryLayout<Int32>.size, "hw.memsize": MemoryLayout<UInt64>.size,
            "hw.pagesize": MemoryLayout<Int32>.size, "kern.boottime": MemoryLayout<timeval>.size])
}

private func preserves_independent_kernel_failure() {
    // Goal: a refused field keeps its kernel error without hiding other measured facts.
    // Method: refuse one fixed key and read the complete snapshot.
    let kernel = ComputerInventoryKernelFake()
    kernel.fail(key: "hw.product", errno: EPERM)

    let snapshot = ComputerInventoryReader(read: kernel.read).snapshot()

    precondition(snapshot.modelIdentifier.value == nil)
    precondition(snapshot.modelIdentifier.key == "hw.product")
    precondition(snapshot.modelIdentifier.failure == .kernel(errno: EPERM))
    precondition(snapshot.chipName.value == "Apple M5 Pro")
    precondition(snapshot.memoryBytes.value == 51_539_607_552)
    precondition(kernel.requests.count == 11)
}

private func preserves_a_full_capacity_inventory_string() {
    // Goal: the string buffer accepts the exact valid bound without truncation.
    // Method: provide 511 ASCII characters followed by the final terminator.
    let kernel = ComputerInventoryKernelFake()
    kernel.seed(key: "hw.product", bytes: [UInt8](repeating: 65, count: 511) + [0], count: 512)

    let snapshot = ComputerInventoryReader(read: kernel.read).snapshot()

    precondition(snapshot.modelIdentifier.value == String(repeating: "A", count: 511))
}

private func preserves_whole_second_boot_time() {
    // Goal: a valid kernel boot timestamp needs no fractional microseconds.
    // Method: return a positive whole-second timeval at its exact native width.
    let kernel = ComputerInventoryKernelFake()
    let value = timeval(tv_sec: 1_700_000_000, tv_usec: 0)
    kernel.seed(key: "kern.boottime", bytes: inventoryBytes(value), count: MemoryLayout<timeval>.size)

    let reading = ComputerInventoryReader(read: kernel.read).snapshot().bootTime

    precondition(reading.value == Date(timeIntervalSince1970: 1_700_000_000))
    precondition(reading.failure == nil)
}

private func rejects_inventory_string(bytes: [UInt8], count: Int,
                                      reason: ComputerInventoryValidation) {
    // Goal: malformed strings remain unavailable instead of truncating or repairing their bytes.
    // Method: give the same native field each malformed reply and compare its typed reason.
    let kernel = ComputerInventoryKernelFake()
    kernel.seed(key: "hw.product", bytes: bytes, count: count)

    let reading = ComputerInventoryReader(read: kernel.read).snapshot().modelIdentifier

    precondition(reading.value == nil, "String bytes \(bytes) count \(count)")
    precondition(reading.failure == .invalid(reason), "String bytes \(bytes) count \(count)")
}

private func rejects_inventory_count(bytes: [UInt8], count: Int,
                                     reason: ComputerInventoryValidation) {
    // Goal: counts reject incorrect sizes, zero and negative signed kernel values.
    // Method: return the malformed native integer without changing its key or buffer capacity.
    let kernel = ComputerInventoryKernelFake()
    kernel.seed(key: "hw.physicalcpu_max", bytes: bytes, count: count)

    let reading = ComputerInventoryReader(read: kernel.read).snapshot().physicalCores

    precondition(reading.value == nil, "Count bytes \(bytes) count \(count)")
    precondition(reading.failure == .invalid(reason), "Count bytes \(bytes) count \(count)")
}

private func rejects_inventory_memory(bytes: [UInt8], count: Int,
                                      reason: ComputerInventoryValidation) {
    // Goal: installed memory rejects malformed widths and zero without a substitute capacity.
    // Method: return the malformed native quantity at the kernel boundary.
    let kernel = ComputerInventoryKernelFake()
    kernel.seed(key: "hw.memsize", bytes: bytes, count: count)

    let reading = ComputerInventoryReader(read: kernel.read).snapshot().memoryBytes

    precondition(reading.value == nil, "Memory bytes \(bytes) count \(count)")
    precondition(reading.failure == .invalid(reason), "Memory bytes \(bytes) count \(count)")
}

private func rejects_inventory_boot(value: timeval, count: Int,
                                    reason: ComputerInventoryValidation) {
    // Goal: invalid timeval widths and components never turn into an apparently valid boot date.
    // Method: seed malformed native timeval replies and compare the independent field failure.
    let kernel = ComputerInventoryKernelFake()
    kernel.seed(key: "kern.boottime", bytes: inventoryBytes(value), count: count)

    let reading = ComputerInventoryReader(read: kernel.read).snapshot().bootTime

    precondition(reading.value == nil, "Boot seconds \(value.tv_sec) micros \(value.tv_usec)")
    precondition(reading.failure == .invalid(reason),
        "Boot seconds \(value.tv_sec) micros \(value.tv_usec) count \(count)")
}

@MainActor
private func reads_inventory_once_until_explicit_refresh() {
    // Goal: entering the pane repeatedly does no polling, while refresh takes one new snapshot.
    // Method: change the fake's source and clock between repeated entries and explicit refresh.
    let kernel = ComputerInventoryKernelFake()
    var instant = Date(timeIntervalSince1970: 1_800_000_000)
    let model = ComputerInventoryModel(reader: ComputerInventoryReader(read: kernel.read,
        now: { instant }))
    precondition(model.snapshot == nil)
    precondition(kernel.requests.isEmpty)

    model.loadIfNeeded()
    let first = model.snapshot
    kernel.seed(key: "hw.product", bytes: Array("Mac99,1\0".utf8), count: 8)
    instant = instant.addingTimeInterval(10)
    model.loadIfNeeded()

    precondition(model.snapshot == first)
    precondition(kernel.requests.count == 11)

    model.refresh()

    precondition(model.snapshot?.modelIdentifier.value == "Mac99,1")
    precondition(model.snapshot?.readAt == instant)
    precondition(kernel.requests.count == 22)
}

@MainActor
private func replaces_successful_inventory_with_refresh_failures() {
    // Goal: refresh errors replace previous success rather than displaying stale facts as current.
    // Method: refuse every kernel key after a successful initial snapshot, then refresh once.
    let kernel = ComputerInventoryKernelFake()
    let model = ComputerInventoryModel(reader: ComputerInventoryReader(read: kernel.read))
    model.loadIfNeeded()
    kernel.failAll(errno: EIO)

    model.refresh()

    precondition(model.snapshot?.modelIdentifier.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.chipName.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.macOSVersion.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.macOSBuild.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.kernelRelease.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.architecture.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.physicalCores.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.logicalCores.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.memoryBytes.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.pageBytes.failure == .kernel(errno: EIO))
    precondition(model.snapshot?.bootTime.failure == .kernel(errno: EIO))
    precondition(kernel.requests.count == 22)
}

@MainActor
private func keeps_initial_failure_until_explicit_refresh_recovers() {
    // Goal: an unavailable first inventory stays visible until refresh explicitly rereads it.
    // Method: restore the failed source after initial entry, then compare entry with refresh.
    let kernel = ComputerInventoryKernelFake()
    kernel.fail(key: "hw.product", errno: ENOENT)
    let model = ComputerInventoryModel(reader: ComputerInventoryReader(read: kernel.read))
    model.loadIfNeeded()
    kernel.seed(key: "hw.product", bytes: Array("Mac17,8\0".utf8), count: 8)

    model.loadIfNeeded()

    precondition(model.snapshot?.modelIdentifier.failure == .kernel(errno: ENOENT))
    precondition(kernel.requests.count == 11)

    model.refresh()

    precondition(model.snapshot?.modelIdentifier.value == "Mac17,8")
    precondition(kernel.requests.count == 22)
}

private final class ComputerInventoryKernelFake {
    private var responses: [String: [UInt8]] = [:]
    private var counts: [String: Int] = [:]
    private var failures: [String: Int32] = [:]
    private(set) var requests: [(key: String, capacity: Int)] = []

    init() { reset() }

    func reset() {
        requests = []
        counts = [:]
        failures = [:]
        responses = [
            "hw.product": Array("Mac17,8\0".utf8),
            "machdep.cpu.brand_string": Array("Apple M5 Pro\0".utf8),
            "kern.osproductversion": Array("26.2\0".utf8),
            "kern.osversion": Array("25C99\0".utf8),
            "kern.osrelease": Array("25.2.0\0".utf8),
            "hw.machine": Array("arm64\0".utf8),
            "hw.physicalcpu_max": inventoryBytes(Int32(18)),
            "hw.logicalcpu_max": inventoryBytes(Int32(18)),
            "hw.memsize": inventoryBytes(UInt64(51_539_607_552)),
            "hw.pagesize": inventoryBytes(Int32(16_384)),
            "kern.boottime": inventoryBytes(timeval(tv_sec: 1_700_000_000, tv_usec: 250_000))]
    }

    func seed(key: String, bytes: [UInt8], count: Int) {
        responses[key] = bytes
        counts[key] = count
        failures.removeValue(forKey: key)
    }

    func fail(key: String, errno: Int32) { failures[key] = errno }

    func failAll(errno: Int32) {
        for key in responses.keys { failures[key] = errno }
    }

    func read(key: String, into buffer: UnsafeMutableRawBufferPointer) -> ComputerInventoryReadResult {
        requests.append((key: key, capacity: buffer.count))
        if let code = failures[key] { return .unavailable(errno: code) }
        guard let bytes = responses[key] else { return .unavailable(errno: ENOENT) }
        buffer.copyBytes(from: bytes.prefix(buffer.count))
        return .read(bytes: counts[key] ?? bytes.count)
    }
}

private func inventoryBytes<Value>(_ value: Value) -> [UInt8] {
    withUnsafeBytes(of: value) { Array($0) }
}
