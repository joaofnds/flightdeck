import Foundation

struct LiveAppGroups {
    let rows: [LiveAppProcessGroup]
    let loadedMembers: UInt32
    let totalMembers: UInt32

    init(detail: LiveAppDetailPage) {
        precondition(detail.processes.count <= 512)
        var groups: [Data?: LiveAppGroupBuilder] = [:]
        for process in detail.processes {
            let key = process.executableName?.bytes
            var group = groups[key] ?? LiveAppGroupBuilder(
                key: key, displayName: process.executableName?.display ?? "Unavailable executable")
            group.add(process)
            groups[key] = group
        }
        rows = groups.values.map { $0.finish(elapsedNs: detail.header.elapsedNs?.value) }
            .sorted { left, right in
                if left.displayName != right.displayName { return left.displayName < right.displayName }
                return (left.key ?? Data()).lexicographicallyPrecedes(right.key ?? Data())
            }
        loadedMembers = UInt32(detail.processes.count)
        totalMembers = detail.app.members
    }

    var coverage: String {
        if loadedMembers < totalMembers {
            return "\(loadedMembers) of \(totalMembers) processes loaded · group subtotals cover this page"
        }

        return "\(loadedMembers) processes loaded · membership remains uncertain"
    }
}

struct LiveAppProcessGroup: Identifiable {
    let key: Data?
    let displayName: String
    let instances: UInt32
    let cpuNs: LiveAppMeasure
    let energyNj: LiveAppMeasure
    let residentBytes: LiveAppMeasure
    let footprintBytes: LiveAppMeasure
    let cpuPercentOneCore: Double?
    let energyJoules: Double?
    let energyWatts: Double?
    var id: Data? { key }
}

private struct LiveAppGroupBuilder {
    let key: Data?
    let displayName: String
    private var instances: UInt32 = 0
    private var cpuNs = LiveAppSubtotal()
    private var energyNj = LiveAppSubtotal()
    private var residentBytes = LiveAppSubtotal()
    private var footprintBytes = LiveAppSubtotal()

    init(key: Data?, displayName: String) {
        self.key = key
        self.displayName = displayName
    }

    mutating func add(_ process: LiveAppProcess) {
        instances += 1
        if let reason = process.appAttribution {
            cpuNs.add(LiveAppAmount(known: nil, unavailable: reason))
            energyNj.add(LiveAppAmount(known: nil, unavailable: reason))
        } else {
            cpuNs.add(process.cpuNs)
            energyNj.add(process.energyNj)
        }
        if process.state == .live {
            residentBytes.add(process.residentBytes)
            footprintBytes.add(process.footprintBytes)
        }
    }

    func finish(elapsedNs: UInt64?) -> LiveAppProcessGroup {
        let elapsed = elapsedNs.flatMap { $0 > 0 ? Double($0) : nil }
        let cpu = cpuNs.measure.knownValue.map { Double($0) }
        let energy = energyNj.measure.knownValue.map { Double($0) }

        return LiveAppProcessGroup(key: key, displayName: displayName, instances: instances,
            cpuNs: cpuNs.measure, energyNj: energyNj.measure, residentBytes: residentBytes.measure,
            footprintBytes: footprintBytes.measure,
            cpuPercentOneCore: elapsed.flatMap { time in cpu.map { $0 / time * 100 } },
            energyJoules: energy.map { $0 / 1_000_000_000 },
            energyWatts: elapsed.flatMap { time in energy.map { $0 / time } })
    }
}

private struct LiveAppSubtotal {
    private var known: UInt64 = 0
    private var measured: UInt32 = 0
    private var unavailable: UInt32 = 0
    private var overflowed = false
    var measure: LiveAppMeasure {
        LiveAppMeasure(known: LiveAppCounter(known), measuredMembers: measured,
            unavailableMembers: unavailable, overflowed: overflowed)
    }

    mutating func add(_ amount: LiveAppAmount) {
        guard let value = amount.known?.value else {
            unavailable += 1
            return
        }

        let sum = known.addingReportingOverflow(value)
        if sum.overflow {
            unavailable += 1
            overflowed = true
        } else {
            known = sum.partialValue
            measured += 1
        }
    }
}
