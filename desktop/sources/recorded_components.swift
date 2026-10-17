import SwiftUI

enum RecordedComponent: String, CaseIterable {
    case cpu = "CPU & processes"
    case gpu = "GPU"
    case energy = "Energy"
    case network = "Network"
}

struct RecordedComponentData {
    let context: RecordedChartContext
    let theme: FlightdeckTheme

    var cpu: TimeSeriesLine { line("CPU activity", unit: "%", color: theme.cpuSeries, metric: \.cpuPercent) }
    var gpuActivity: TimeSeriesLine {
        line("GPU activity", unit: "%", color: theme.gpuPowerSeries, metric: \.gpuActivePercent)
    }
    var gpuActivityStyle: TimeSeriesStyle {
        let maximum = gpuActivity.values.compactMap { $0 }.filter(\.isFinite).max() ?? 100

        return TimeSeriesStyle(unit: "%", minimum: 0, maximum: max(100, maximum), fill: 0.1)
    }
    var gpuPower: TimeSeriesLine {
        line("GPU power", unit: "W", color: theme.gpuPowerSeries, metric: \.gpuWatts)
    }
    var energy: [TimeSeriesLine] {
        [line("System power", unit: "W", color: theme.systemPowerSeries, metric: \.powerWatts), gpuPower]
    }
    var network: [TimeSeriesLine] {
        [line("Received", unit: "KiB/s", color: theme.networkReceivedSeries,
              metric: \.networkRxBps, scale: 1024),
         line("Sent", unit: "KiB/s", color: theme.networkSentSeries,
              metric: \.networkTxBps, scale: 1024, negative: true)]
    }
    var networkPackets: [TimeSeriesLine] {
        [line("Received", unit: "packets/s", color: theme.networkReceivedPacketsSeries,
              metric: \.networkRxPacketsPerSecond),
         line("Sent", unit: "packets/s", color: theme.networkSentPacketsSeries,
              metric: \.networkTxPacketsPerSecond, negative: true, pattern: .dashed)]
    }
    var networkErrors: [TimeSeriesLine] {
        [line("Receive errors", unit: "errors/s", color: theme.networkReceiveErrorsSeries,
              metric: \.networkRxErrorsPerSecond),
         line("Send errors", unit: "errors/s", color: theme.networkSendErrorsSeries,
              metric: \.networkTxErrorsPerSecond, pattern: .dashed)]
    }

    private func line(_ name: String, unit: String, color: Color, metric: KeyPath<Metric, Double?>,
                      scale: Double = 1, negative: Bool = false,
                      pattern: TimeSeriesStrokePattern = .solid) -> TimeSeriesLine {
        TimeSeriesLine(name: name, color: color,
            values: context.selection.trends.map { $0[keyPath: metric].map { $0 / scale } },
            unit: unit, negative: negative, strokePattern: pattern)
    }
}

@MainActor
extension RecordingModel {
    func enterComponent(_ component: RecordedComponent) {
        if component == .cpu { enterProcesses() }
        else { route = component.rawValue }
    }
}

struct RecordedComponentPane: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    let component: RecordedComponent
    @State private var inspectedID: String?
    @State private var inspecting = false

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            if let selection = model.selection {
                let data = RecordedComponentData(context: RecordedChartContext(selection: selection),
                    theme: theme)
                if selection.selected == nil {
                    Label("No recorded tick at this second", systemImage: "clock.badge.questionmark")
                        .foregroundStyle(theme.muted)
                }
                switch component {
                case .cpu:
                    chart("CPU activity", data: data, lines: [data.cpu], style: activityStyle,
                        explanation: "Busy scheduler ticks divided by total ticks across the recorded logical CPUs. An interval needs compatible consecutive observations.")
                    LogicalCPUActivity(readings: model.cpuDetail, busy: model.busy)
                case .gpu:
                    ViewThatFits(in: .horizontal) {
                        HStack(alignment: .top, spacing: theme.panelGap) {
                            gpuActivity(data).frame(minWidth: 330)
                            gpuPower(data).frame(minWidth: 330)
                        }
                        VStack(spacing: theme.panelGap) { gpuActivity(data); gpuPower(data) }
                    }
                case .energy:
                    chart("Recorded power", data: data, lines: data.energy,
                        style: TimeSeriesStyle(unit: "W", fill: 0.1),
                        explanation: "System power is the recorded SMC reading. GPU power uses recorded energy over its measured interval. The two series are independent and are not added together.")
                case .network:
                    chart("External interfaces", data: data, lines: data.network,
                        style: TimeSeriesStyle(unit: "KiB/s", fill: 0.1),
                        explanation: "Byte rates across recorded non-loopback interfaces. Sent traffic is mirrored below zero; readouts remain positive. VPN and tunnel traffic may count the same bytes more than once.")
                    ViewThatFits(in: .horizontal) {
                        HStack(alignment: .top, spacing: theme.panelGap) {
                            networkPackets(data).frame(minWidth: 330)
                            networkErrors(data).frame(minWidth: 330)
                        }
                        VStack(spacing: theme.panelGap) { networkPackets(data); networkErrors(data) }
                    }
                }
                Text(sourceNote).font(.callout).foregroundStyle(theme.muted)
            } else {
                Text(model.busy ? "Reading selected tick…" : "No observation selected.")
                    .foregroundStyle(theme.muted)
            }
        }
        .onChange(of: model.selection?.selected?.publication) { _, _ in resetInspection() }
        .onChange(of: model.selection?.selected?.run) { _, _ in resetInspection() }
        .onChange(of: model.selection?.selected?.boot) { _, _ in resetInspection() }
        .onChange(of: model.selection?.second) { _, _ in resetInspection() }
    }

    private var activityStyle: TimeSeriesStyle {
        TimeSeriesStyle(unit: "%", minimum: 0, maximum: 100, fill: 0.1)
    }

    private var sourceNote: String {
        switch component {
        case .cpu: "Logical CPU activity uses independent scheduler counters. Core topology and clock frequencies are unavailable."
        case .gpu: "Activity and power use separate recorded counters. Unavailable intervals remain gaps."
        case .energy: "These readings do not identify CPU power or attribute system power to apps."
        case .network: "Counter rates use measured continuous elapsed time and require an unchanged interface set."
        }
    }

    private func resetInspection() { inspectedID = nil; inspecting = false }

    private func gpuActivity(_ data: RecordedComponentData) -> some View {
        chart("GPU activity", data: data, lines: [data.gpuActivity], style: data.gpuActivityStyle,
            explanation: "Time in recorded active GPU states divided by the measured continuous interval. Requires compatible observations and complete matching active states.")
    }

    private func gpuPower(_ data: RecordedComponentData) -> some View {
        chart("GPU power", data: data, lines: [data.gpuPower],
            style: TimeSeriesStyle(unit: "W", fill: 0.1),
            explanation: "Recorded GPU energy change divided by the measured continuous interval.")
    }

    private func networkPackets(_ data: RecordedComponentData) -> some View {
        chart("Packets", data: data, lines: data.networkPackets,
            style: TimeSeriesStyle(unit: "packets/s", fill: 0.1),
            explanation: "Packet rates across unchanged non-loopback interfaces over the measured continuous interval. Sent packets are mirrored below zero; readouts remain positive. VPN and tunnel traffic may count the same packets more than once.")
    }

    private func networkErrors(_ data: RecordedComponentData) -> some View {
        chart("Network errors", data: data, lines: data.networkErrors,
            style: TimeSeriesStyle(unit: "errors/s"),
            explanation: "Receive and send error-counter changes per second across unchanged non-loopback interfaces. Each direction remains independent. A missing or reset counter leaves a gap for that direction.")
    }

    private func chart(_ title: String, data: RecordedComponentData, lines: [TimeSeriesLine],
                       style: TimeSeriesStyle, explanation: String) -> some View {
        RecordedResourceChart(title: title, context: data.context, lines: lines, style: style,
            inspectedID: $inspectedID, inspecting: $inspecting, explanation: explanation)
    }
}
