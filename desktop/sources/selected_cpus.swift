import Foundation

struct SelectedCPUReadings: Sendable {
    static let cpusMax = 1024
    let observation: Observation
    let previousObservation: Observation?
    let intervalAvailable: Bool
    let unavailableReason: String?
    let cpus: [LogicalCPUReading]

    init(payload: CPUDetailPayload, selected: Observation) throws {
        guard payload.observation.identity == selected.identity,
              payload.observation.runTick == selected.runTick,
              payload.observation.wallNs == selected.wallNs else {
            throw CPUDetailError.observationMismatch
        }

        guard payload.cpus.count <= Self.cpusMax else { throw CPUDetailError.domain }

        var seen = Set<UInt32>()
        for reading in payload.cpus {
            guard reading.cpu < UInt32(payload.cpus.count), seen.insert(reading.cpu).inserted else {
                throw CPUDetailError.domain
            }

            if let value = reading.busyPercent {
                guard value.isFinite, (0...100).contains(value) else {
                    throw CPUDetailError.percentage
                }
            }
        }

        try Self.validateInterval(payload)

        observation = payload.observation
        previousObservation = payload.previousObservation
        intervalAvailable = payload.intervalAvailable
        unavailableReason = payload.unavailableReason
        cpus = payload.cpus.sorted { $0.cpu < $1.cpu }
    }

    private static func validateInterval(_ payload: CPUDetailPayload) throws {
        if let reason = payload.unavailableReason {
            guard reason.utf8.count <= 96 else { throw CPUDetailError.interval }
        }
        if let previous = payload.previousObservation {
            guard previous.run == payload.observation.run,
                  previous.boot == payload.observation.boot else {
                throw CPUDetailError.interval
            }
        }
        guard payload.intervalAvailable else {
            guard payload.cpus.allSatisfy({ $0.busyPercent == nil }) else {
                throw CPUDetailError.interval
            }

            return
        }

        guard !payload.cpus.isEmpty, payload.unavailableReason == nil,
              let previous = payload.previousObservation,
              previous.publication < UInt64.max, previous.runTick < UInt64.max,
              previous.publication + 1 == payload.observation.publication,
              previous.runTick + 1 == payload.observation.runTick,
              previous.wallNs < payload.observation.wallNs,
              payload.observation.wallNs / 1_000_000_000 <= previous.wallNs / 1_000_000_000 + 1 else {
            throw CPUDetailError.interval
        }
    }
}

struct CPUDetailPayload: Decodable, Sendable {
    let observation: Observation
    let previousObservation: Observation?
    let intervalAvailable: Bool
    let unavailableReason: String?
    let cpus: [LogicalCPUReading]
}

struct LogicalCPUReading: Decodable, Equatable, Identifiable, Sendable {
    let cpu: UInt32
    let busyPercent: Double?
    var id: UInt32 { cpu }
}

enum CPUDetailError: Error, LocalizedError, Equatable {
    case observationMismatch
    case domain
    case percentage
    case interval

    var errorDescription: String? {
        switch self {
        case .observationMismatch: "CPU activity does not match the selected observation."
        case .domain: "CPU activity has an invalid logical CPU domain."
        case .percentage: "CPU activity has an invalid percentage."
        case .interval: "CPU activity has an invalid measurement interval."
        }
    }
}
