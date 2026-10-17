import Foundation

struct SelectedFanReadings: Sendable {
    let observation: Observation
    let fan0: Double?
    let fan1: Double?
}

extension SelectedFanReadings {
    // The collector's FANS_MAX bounds the single-digit SMC fan channel keys.
    static let channelsMax = 10

    init(page: TablePage, selected: Observation) throws {
        guard page.name == "fans", page.offset == 0,
              page.total <= UInt64(Self.channelsMax), page.rows.count == Int(page.total),
              page.columns == ["publication", "fan", "rpm"] else {
            throw RecordingError(message: "Invalid selected fan page.")
        }

        guard page.observation.identity == selected.identity,
              page.observation.runTick == selected.runTick,
              page.observation.wallNs == selected.wallNs else {
            throw RecordingError(message: "Fan readings do not match the selected observation.")
        }

        var channels = [Double?](repeating: nil, count: Self.channelsMax)
        var seen = Set<UInt32>()
        for row in page.rows {
            guard row.count == 3, let publication = row[0].flatMap(UInt64.init),
                  publication == selected.publication, let channel = row[1].flatMap(UInt32.init),
                  channel < UInt32(page.total), seen.insert(channel).inserted else {
                throw RecordingError(message: "Invalid selected fan channel.")
            }

            channels[Int(channel)] = try Self.readRPM(row[2])
        }

        self.init(observation: selected, fan0: channels[0], fan1: channels[1])
    }

    private static func readRPM(_ cell: String?) throws -> Double? {
        guard let cell else { return nil }

        guard let value = Double(cell) else {
            throw RecordingError(message: "Invalid selected fan RPM.")
        }

        return value.isFinite && value >= 0 ? value : nil
    }
}
