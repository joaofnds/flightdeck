import SwiftUI

struct FanRPMReadings: Decodable, Sendable {
    static let channelsMax = SelectedFanReadings.channelsMax
    let values: [Double?]

    init(from decoder: any Decoder) throws {
        var container = try decoder.unkeyedContainer()
        var values: [Double?] = []
        values.reserveCapacity(Self.channelsMax)

        for _ in 0..<Self.channelsMax {
            if container.isAtEnd { self.values = values; return }

            let value = try container.decodeIfPresent(Double.self)
            values.append(value.flatMap { gauge in
                guard gauge.isFinite, gauge >= 0 else { return nil }

                return gauge == 0 ? 0 : gauge
            })
        }

        guard container.isAtEnd else {
            throw DecodingError.dataCorruptedError(in: container,
                debugDescription: "Fan history exceeds ten channels.")
        }

        self.values = values
    }
}

struct FanHistoryData {
    private static let strokePatterns: [TimeSeriesStrokePattern] = [
        .solid, .dashed, .dotted, .custom([12,4]), .custom([8,3,2,3]),
        .custom([8,3,2,3,2,3]), .custom([3,2]), .custom([12,3,4,3]),
        .custom([3,2,3,2,3,6]), .custom([1,2,1,6]),
    ]

    let context: RecordedChartContext
    let lines: [TimeSeriesLine]

    init?(context: RecordedChartContext, theme: FlightdeckTheme) {
        guard context.selection.trends.count <= TimeSeriesModel.observationsMax else { return nil }

        precondition(theme.fanSeries.count == FanRPMReadings.channelsMax)
        precondition(Self.strokePatterns.count == FanRPMReadings.channelsMax)

        let channels = context.selection.trends.reduce(0) {
            max($0, $1.fanRpm?.values.count ?? 0)
        }

        self.context = context
        self.lines = (0..<channels).map { channel in
            TimeSeriesLine(name: "Fan \(channel)", color: theme.fanSeries[channel],
                values: context.selection.trends.map { point -> Double? in
                    guard let readings = point.fanRpm, channel < readings.values.count else {
                        return nil
                    }

                    return readings.values[channel]
                }, unit: "RPM", stack: false, negative: false,
                strokePattern: Self.strokePatterns[channel])
        }
    }

    var selectedMessage: String {
        guard let selected = context.selection.selected, let summary = context.selection.summary,
              summary.identity == selected.identity, let readings = summary.fanRpm else {
            return "Fan readings unavailable at the selected observation."
        }

        if readings.values.isEmpty { return "No fans reported at the selected observation." }

        if readings.values.allSatisfy({ $0 == nil }) {
            return "RPM unavailable for all reported fan channels at the selected observation."
        }

        return "SMC fan channel numbers do not identify physical positions."
    }
}

struct RecordedFanHistory: View {
    @Environment(\.flightdeckTheme) private var theme
    let context: RecordedChartContext
    @Binding var inspectedID: String?
    @Binding var inspecting: Bool

    static let explanation = "Available fan readings from the last fifteen minutes. Values are SMC channel gauges; channel numbers do not identify CPU, GPU, left or right fan positions. Missing or invalid readings remain gaps."

    var body: some View {
        if let data = FanHistoryData(context: context, theme: theme) {
            if data.lines.isEmpty {
                Panel(title: "Fan speed", explanation: Self.explanation) {
                    Text(data.selectedMessage).foregroundStyle(theme.muted)
                }
            } else {
                VStack(alignment: .leading, spacing: theme.fieldGap) {
                    Self.chart(data, inspectedID: $inspectedID, inspecting: $inspecting)
                    Text(data.selectedMessage).font(.caption).foregroundStyle(theme.muted)
                }
            }
        } else {
            Panel(title: "Fan speed", explanation: Self.explanation) {
                Text("Chart readings exceed the display bounds.").foregroundStyle(theme.muted)
            }
        }
    }

    static func chart(_ data: FanHistoryData, inspectedID: Binding<String?>,
                      inspecting: Binding<Bool>) -> RecordedResourceChart {
        RecordedResourceChart(title: "Fan speed", context: data.context, lines: data.lines,
            style: TimeSeriesStyle(unit: "RPM", stack: false, minimum: 0, fill: 0.08),
            inspectedID: inspectedID, inspecting: inspecting, explanation: explanation)
    }
}
