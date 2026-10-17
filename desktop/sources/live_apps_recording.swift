import Foundation
import SwiftUI

struct LiveAppsRequest: Equatable, Sendable {
    let key: LiveAppKeyIdentity?
    let appsOffset: UInt32
    let membersOffset: UInt32

    static func validOffset(requested: UInt32, total: UInt32) -> UInt32 {
        requested < total ? requested : 0
    }
}

struct LiveAppsRead: Sendable {
    let key: LiveAppKeyIdentity?
    let list: LiveAppListPage
    let detail: LiveAppDetailPage?
    let history: LiveAppHistoryPayload?
    let notice: String?
}

struct LiveAppHistoryPayload: Decodable, Sendable {
    let selected: LiveAppObservation
    let contextSeconds: UInt32
    let points: [LiveAppHistoryPoint]
    let coverage: LiveAppHistoryCoverage
    private enum CodingKeys: String, CodingKey { case selected, contextSeconds, points, coverage }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        selected = try container.decode(LiveAppObservation.self, forKey: .selected)
        contextSeconds = try container.decode(UInt32.self, forKey: .contextSeconds)
        coverage = try container.decode(LiveAppHistoryCoverage.self, forKey: .coverage)
        var input = try container.nestedUnkeyedContainer(forKey: .points)
        var points: [LiveAppHistoryPoint] = []
        points.reserveCapacity(min(TimeSeriesModel.observationsMax, input.count ?? 0))
        while !input.isAtEnd {
            guard points.count < TimeSeriesModel.observationsMax else {
                throw LiveAppDataError.populationBound
            }

            points.append(try input.decode(LiveAppHistoryPoint.self))
        }
        guard contextSeconds == 900, let wall = selected.wallNs?.value,
              coverage.cacheBytes <= coverage.cacheBytesMax,
              coverage.observationsRetained <= 4096 else { throw LiveAppDataError.populationBound }

        let second = wall / 1_000_000_000
        let first = second > 899 ? second - 899 : 0
        guard points.allSatisfy({ point in
            guard let wall = point.observation.wallNs?.value else { return false }

            return (first...second).contains(wall / 1_000_000_000)
        }), Set(points.map(\.id)).count == points.count else {
            throw LiveAppDataError.invalidQuantity
        }

        self.points = points
    }

    @MainActor
    func view(key: LiveAppKeyIdentity, theme: FlightdeckTheme) -> LiveAppHistoryView? {
        guard let wall = selected.wallNs?.value else { return nil }

        let second = Double(wall / 1_000_000_000)
        let domain = (second - 899)...(second + 1)
        let samples = points.compactMap { point -> TimeSeriesSample? in
            guard let wall = point.observation.wallNs?.value else { return nil }

            return TimeSeriesSample(id: point.id, time: Double(wall) / 1e9,
                segment: point.observation.stamp.boot.text + ":" + point.observation.stamp.run.text
                    + ":" + String(point.segment.value))
        }
        guard samples.count == points.count else { return nil }

        let labels = [domain.lowerBound, domain.lowerBound + 450, domain.upperBound].map {
            Date(timeIntervalSince1970: $0).formatted(.dateTime.hour().minute().second())
        }
        let coverageText = "\(points.count) captured App observations · membership coverage is uncertain"
            + (coverage.retentionLimited ? " · history was shortened by its cache limit" : "")

        return LiveAppHistoryView(key: key,
            cpu: model("CPU", samples: samples, domain: domain, color: theme.cpuSeries,
                values: points.map(\.cpuPercentOneCore), unit: "%"),
            resident: model("Resident", samples: samples, domain: domain, color: theme.memoryActiveSeries,
                values: points.map { $0.residentBytes?.knownValue.map { Double($0) / 1_073_741_824 } },
                unit: "GiB"),
            footprint: model("Footprint", samples: samples, domain: domain, color: theme.memoryWiredSeries,
                values: points.map { $0.footprintBytes?.knownValue.map { Double($0) / 1_073_741_824 } },
                unit: "GiB"),
            power: model("Average power", samples: samples, domain: domain, color: theme.systemPowerSeries,
                values: points.map(\.energyWatts), unit: "W"),
            selectedTime: Double(wall) / 1e9, selectedID: selected.id, labels: labels, coverage: coverageText)
    }

    private func model(_ name: String, samples: [TimeSeriesSample], domain: ClosedRange<Double>,
                       color: Color, values: [Double?], unit: String) -> TimeSeriesModel? {
        TimeSeriesModel(samples: samples,
            lines: [TimeSeriesLine(name: name, color: color, values: values, unit: unit)], domain: domain)
    }
}

struct LiveAppHistoryPoint: Decodable, Sendable {
    let observation: LiveAppObservation
    let segment: LiveAppCounter
    let elapsedNs: LiveAppCounter?
    let members: UInt32?
    let membershipComplete: Bool
    let cpuNs: LiveAppMeasure?
    let energyNj: LiveAppMeasure?
    let residentBytes: LiveAppMeasure?
    let footprintBytes: LiveAppMeasure?
    let cpuPercentOneCore: Double?
    let energyJoules: Double?
    let energyWatts: Double?
    var id: String { observation.id }
}

struct LiveAppHistoryCoverage: Decodable, Sendable {
    let retentionLimited: Bool
    let observationsRetained: UInt32
    let cacheBytes: UInt64
    let cacheBytesMax: UInt64
}

extension LiveAppIdentityWords {
    var text: String { String(format: "%016llx%016llx", first, second) }
}

extension LiveAppObservation {
    var id: String { "\(stamp.boot.text):\(stamp.run.text):\(stamp.publication.value)" }
    var recordedIdentity: RecordedObservationIdentity {
        RecordedObservationIdentity(run: stamp.run.text, boot: stamp.boot.text,
            publication: stamp.publication.value)
    }

    func matches(_ selected: Observation) -> Bool {
        recordedIdentity == selected.identity && runTick.value == selected.runTick
            && wallNs?.value == selected.wallNs
    }
}
