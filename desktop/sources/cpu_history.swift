import Foundation

struct SelectedCPUHistory: Sendable {
    static let observationsMax = 4096
    static let cpusPerPage: UInt32 = 32
    let observation: Observation
    let firstNs: UInt64
    let endNs: UInt64
    let offset: UInt32
    let total: UInt32
    let retentionLimited: Bool
    let restorationError: String?
    let observations: [CPUHistoryObservation]
    let cpus: [LogicalCPUHistoryRow]

    init(payload: CPUHistoryPayload, selection: Selection) throws {
        guard let selected = selection.selected,
              payload.observation.identity == selected.identity,
              payload.observation.runTick == selected.runTick,
              payload.observation.wallNs == selected.wallNs else {
            throw CPUHistoryError.observationMismatch
        }

        try Self.validateContext(payload, selection: selection)
        try Self.validatePage(payload)
        let positions = try Self.validateObservations(payload, selection: selection)
        try Self.validateIntervals(payload, positions: positions)
        try Self.validateRows(payload)
        if let message = payload.restorationError {
            guard !message.isEmpty, message.count <= 512 else { throw CPUHistoryError.diagnostic }
        }

        observation = payload.observation
        firstNs = payload.firstNs
        endNs = payload.endNs
        offset = payload.offset
        total = payload.total
        retentionLimited = payload.retentionLimited
        restorationError = payload.restorationError
        observations = payload.observations
        cpus = payload.cpus
    }

    private static func validateContext(_ payload: CPUHistoryPayload,
                                        selection: Selection) throws {
        guard selection.contextSeconds == 900, payload.contextSeconds == 900,
              selection.second == payload.observation.wallNs / 1_000_000_000,
              payload.observations.count <= observationsMax,
              payload.observations.count == selection.trends.count,
              !payload.observations.isEmpty else { throw CPUHistoryError.context }

        let first = selection.second >= 899 ? selection.second - 899 : 0
        let end = (selection.second + 1).multipliedReportingOverflow(by: 1_000_000_000)

        guard !end.overflow, payload.firstNs == first * 1_000_000_000,
              payload.endNs == end.partialValue else { throw CPUHistoryError.context }
    }

    private static func validatePage(_ payload: CPUHistoryPayload) throws {
        guard payload.total <= UInt32(SelectedCPUReadings.cpusMax) else {
            throw CPUHistoryError.domain
        }

        guard payload.offset.isMultiple(of: cpusPerPage) else { throw CPUHistoryError.page }

        if payload.total == 0 {
            guard payload.offset == 0, payload.cpus.isEmpty else { throw CPUHistoryError.page }
        } else {
            guard payload.offset < payload.total,
                  payload.cpus.count == Int(min(cpusPerPage, payload.total - payload.offset)) else {
                throw CPUHistoryError.page
            }
        }
    }

    private static func validateObservations(_ payload: CPUHistoryPayload,
        selection: Selection) throws -> [RecordedObservationIdentity: Int] {
        var positions = [RecordedObservationIdentity: Int]()
        positions.reserveCapacity(payload.observations.count)
        var selectedCount: UInt32?

        for (index, sample) in payload.observations.enumerated() {
            let point = sample.observation
            let common = selection.trends[index]

            guard point.identity == common.identity, point.runTick == common.runTick,
                  point.wallNs == common.wallNs,
                  point.wallNs >= payload.firstNs, point.wallNs < payload.endNs,
                  positions.updateValue(index, forKey: point.identity) == nil else {
                throw CPUHistoryError.context
            }

            guard sample.cpuCount <= UInt32(SelectedCPUReadings.cpusMax) else {
                throw CPUHistoryError.domain
            }

            if index > 0 {
                let previous = payload.observations[index - 1].observation

                guard (previous.wallNs, previous.run, previous.boot, previous.publication)
                    < (point.wallNs, point.run, point.boot, point.publication) else {
                    throw CPUHistoryError.context
                }
            }

            if point.identity == payload.observation.identity {
                guard point.runTick == payload.observation.runTick,
                      point.wallNs == payload.observation.wallNs else {
                    throw CPUHistoryError.observationMismatch
                }

                selectedCount = sample.cpuCount
            }
        }

        guard let selectedCount else { throw CPUHistoryError.context }
        guard selectedCount == payload.total else { throw CPUHistoryError.domain }

        return positions
    }

    private static func validateIntervals(_ payload: CPUHistoryPayload,
        positions: [RecordedObservationIdentity: Int]) throws {
        for sample in payload.observations where sample.intervalAvailable {
            let current = sample.observation

            guard sample.cpuCount > 0, let previous = sample.previousObservation,
                  previous.run == current.run, previous.boot == current.boot,
                  previous.publication < UInt64.max, previous.runTick < UInt64.max,
                  previous.publication + 1 == current.publication,
                  previous.runTick + 1 == current.runTick,
                  previous.wallNs < current.wallNs,
                  current.wallNs / 1_000_000_000 <= previous.wallNs / 1_000_000_000 + 1 else {
                throw CPUHistoryError.interval
            }

            if let index = positions[previous.identity] {
                let known = payload.observations[index]

                guard known.cpuCount == sample.cpuCount,
                      known.observation.runTick == previous.runTick,
                      known.observation.wallNs == previous.wallNs else {
                    throw CPUHistoryError.interval
                }
            }
        }
    }

    private static func validateRows(_ payload: CPUHistoryPayload) throws {
        for (index, row) in payload.cpus.enumerated() {
            guard row.cpu == payload.offset + UInt32(index),
                  row.busyPercent.count == payload.observations.count else {
                throw CPUHistoryError.domain
            }

            for (sampleIndex, value) in row.busyPercent.enumerated() {
                guard let value else { continue }

                guard value.isFinite, (0...100).contains(value) else {
                    throw CPUHistoryError.percentage
                }

                let sample = payload.observations[sampleIndex]

                guard sample.intervalAvailable, row.cpu < sample.cpuCount else {
                    throw CPUHistoryError.interval
                }
            }
        }
    }
}

struct CPUHistoryPayload: Decodable, Sendable {
    let observation: Observation
    let contextSeconds: UInt64
    let firstNs: UInt64
    let endNs: UInt64
    let offset: UInt32
    let total: UInt32
    let retentionLimited: Bool
    let observations: [CPUHistoryObservation]
    let cpus: [LogicalCPUHistoryRow]
    var restorationError: String? = nil
}

struct CPUHistoryObservation: Decodable, Sendable {
    let observation: Observation
    let previousObservation: Observation?
    let intervalAvailable: Bool
    let cpuCount: UInt32
}

struct LogicalCPUHistoryRow: Decodable, Identifiable, Sendable {
    let cpu: UInt32
    let busyPercent: [Double?]
    var id: UInt32 { cpu }
}

enum CPUHistoryError: Error, LocalizedError, Equatable {
    case observationMismatch, context, page, domain, interval, percentage, diagnostic

    var errorDescription: String? {
        switch self {
        case .observationMismatch: "CPU history does not match the selected observation."
        case .context: "CPU history does not match the current chart context."
        case .page: "CPU history has an invalid page."
        case .domain: "CPU history has an invalid logical CPU domain."
        case .interval: "CPU history has an invalid measurement interval."
        case .percentage: "CPU history has an invalid percentage."
        case .diagnostic: "CPU history has an invalid restoration diagnostic."
        }
    }
}
