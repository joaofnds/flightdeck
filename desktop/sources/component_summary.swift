import SwiftUI

struct ComponentSummary: View {
    @Environment(\.flightdeckTheme) private var theme
    let component: RecordedComponent
    let metric: Metric?

    var body: some View {
        MetricSummaryGrid {
            switch component {
            case .cpu:
                reading("CPU activity", value: metric?.cpuPercent, unit: "%",
                    note: "All logical CPUs", color: theme.cpuSeries)
                reading("SoC temperature", value: metric?.socCelsius, unit: "°C",
                    note: "Chip sensor", color: theme.temperatureSeries)
            case .gpu:
                reading("GPU activity", value: metric?.gpuActivePercent, unit: "%",
                    note: "Active time", color: theme.gpuActivitySeries)
                reading("GPU power", value: metric?.gpuWatts, unit: "W",
                    note: "Energy per second", color: theme.gpuPowerSeries)
                reading("SoC temperature", value: metric?.socCelsius, unit: "°C",
                    note: "Shared chip sensor", color: theme.temperatureSeries)
            case .energy:
                reading("System power", value: metric?.powerWatts, unit: "W",
                    note: "Whole-machine sensor", color: theme.systemPowerSeries)
                reading("GPU power", value: metric?.gpuWatts, unit: "W",
                    note: "Component energy per second", color: theme.gpuPowerSeries)
                reading("SoC temperature", value: metric?.socCelsius, unit: "°C",
                    note: "Chip sensor", color: theme.temperatureSeries)
            case .network:
                reading("Receive", value: metric?.networkRxBps.map { $0 / 1_000_000 }, unit: "MB/s",
                    note: "External interfaces", color: theme.networkReceivedSeries)
                reading("Send", value: metric?.networkTxBps.map { $0 / 1_000_000 }, unit: "MB/s",
                    note: "External interfaces", color: theme.networkSentSeries)
                reading("Received packets", value: metric?.networkRxPacketsPerSecond, unit: "/s",
                    note: "Packets per second", color: theme.networkReceivedPacketsSeries)
                reading("Sent packets", value: metric?.networkTxPacketsPerSecond, unit: "/s",
                    note: "Packets per second", color: theme.networkSentPacketsSeries)
            }
        }
    }

    private func reading(_ title: String, value: Double?, unit: String,
                         note: String, color: Color) -> some View {
        MetricSummary(title: title, reading: dashboardReading(value, unit: unit),
            note: note, color: color)
    }
}
