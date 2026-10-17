import Charts
import SwiftUI

struct Dashboard: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @StateObject private var inspection = ChartInspectionState()

    private var points: [Metric] { model.selection?.trends ?? [] }
    private var summary: Metric? { model.selection?.summary }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            if model.selection?.selected == nil, !model.busy {
                Label("No recorded tick at this second", systemImage: "clock.badge.questionmark")
                    .foregroundStyle(theme.muted)
            }
            LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: theme.panelGap) {
                linePanel("CPU activity", value: summary?.cpuPercent, unit: "%", color: theme.cpuSeries,
                          explanation: "Busy ticks across all recorded logical CPUs divided by total CPU ticks. Requires consecutive compatible observations.",
                          domain: 0...100, metric: \.cpuPercent)
                memoryPanel
                linePanel("GPU power", value: summary?.gpuWatts, unit: "W", color: theme.gpuPowerSeries,
                          explanation: "Difference in recorded GPU energy divided by the actual continuous elapsed interval. Requires consecutive compatible observations and available IOReport at both ends.",
                          metric: \.gpuWatts)
                linePanel("External interfaces · received", value: summary?.networkRxBps.map { $0 / 1024 },
                          unit: "KiB/s", color: theme.networkReceivedSeries,
                          explanation: "Sum of recorded non-loopback interfaces. VPN and tunnel traffic can be counted more than once. Counter deltas use actual continuous elapsed time.",
                          metric: \.networkRxBps, scale: 1024)
                linePanel("External interfaces · sent", value: summary?.networkTxBps.map { $0 / 1024 },
                          unit: "KiB/s", color: theme.networkSentSeries,
                          explanation: "Sum of sent-byte deltas across an unchanged set of recorded non-loopback interfaces.",
                          metric: \.networkTxBps, scale: 1024)
                linePanel("System power", value: summary?.powerWatts, unit: "W", color: theme.systemPowerSeries,
                          explanation: "Recorded SMC system-power reading. Unread source remains unavailable.",
                          metric: \.powerWatts)
                linePanel("SoC temperature", value: summary?.socCelsius, unit: "°C", color: theme.temperatureSeries,
                          explanation: "Recorded SoC sensor reading. This does not identify separate CPU/GPU sensor positions.",
                          metric: \.socCelsius)
            }
            HStack(spacing: theme.contentPadding) {
                Label("Saved recording", systemImage: "externaldrive")
                if let pressure = summary?.pressure {
                    Text("Memory pressure: " + ([0: "Normal", 1: "Warning", 2: "Critical"][pressure] ?? "Unknown"))
                }
                if let swap = summary?.swapUsedBytes {
                    Text("Swap: " + ByteCountFormatter.string(fromByteCount: Int64(clamping: swap), countStyle: .binary))
                }
                Spacer()
                Button("Inspect processes →", action: model.enterProcesses)
            }.font(.callout).foregroundStyle(theme.muted)
        }
    }

    private var memoryPanel: some View {
        Panel(title: "Memory", explanation: "Recorded active, wired and compressed memory in GiB. These are source categories; cached/free memory remains available in the recorded-memory table.") {
            HStack(alignment: .firstTextBaseline) {
                Text(metricText(summary?.memoryActiveBytes.map { Double($0) / 1_073_741_824 }, unit: "GiB active"))
                    .font(theme.metricFont).monospacedDigit()
                Spacer()
                if let total = summary?.memoryTotalBytes {
                    Text(metricText(Double(total) / 1_073_741_824, unit: "GiB installed", digits: 0))
                        .font(.caption).foregroundStyle(theme.muted)
                }
            }
            Chart(points) { point in
                ForEach(Array(memoryValues(point).enumerated()), id: \.offset) { _, item in
                    if let value = item.1 {
                        AreaMark(x: .value("Time", point.date), y: .value("GiB", Double(value) / 1_073_741_824),
                                 series: .value("Segment", "\(point.segment)-\(item.0)"))
                            .foregroundStyle(by: .value("Memory", item.0))
                    }
                }
            }
            .chartForegroundStyleScale(["Active": theme.memoryActiveSeries, "Wired": theme.memoryWiredSeries, "Compressed": theme.memoryCompressedSeries])
            .chartYAxisLabel("GiB").chartLegend(position: .bottom)
            .frame(height: theme.chartHeight)
            .modifier(ChartInspection(inspection: inspection) { point in
                metricText(point.memoryActiveBytes.map { Double($0) / 1_073_741_824 }, unit: "GiB active")
            })
        }
    }

    private func memoryValues(_ point: Metric) -> [(String, UInt64?)] {
        [("Active", point.memoryActiveBytes), ("Wired", point.memoryWiredBytes),
         ("Compressed", point.memoryCompressedBytes)]
    }

    private func linePanel(_ title: String, value: Double?, unit: String, color: Color,
                           explanation: String, domain: ClosedRange<Double>? = nil,
                           metric: KeyPath<Metric, Double?>, scale: Double = 1) -> some View {
        Panel(title: title, explanation: explanation) {
            Text(metricText(value, unit: unit)).font(theme.metricFont)
                .monospacedDigit()
            Chart(plotSamples(metric, scale: scale)) { sample in
                LineMark(x: .value("Time", sample.date), y: .value(unit, sample.value),
                         series: .value("Segment", sample.segment))
                    .foregroundStyle(color).lineStyle(StrokeStyle(lineWidth: theme.chartStroke))
            }
            .chartYScale(domain: domain ?? 0...max(1, points.compactMap { $0[keyPath: metric] }.max().map { $0 / scale * 1.1 } ?? 1))
            .chartYAxisLabel(unit)
            .frame(height: theme.chartHeight)
            .modifier(ChartInspection(inspection: inspection) { point in
                metricText(point[keyPath: metric].map { $0 / scale }, unit: unit)
            })
        }
    }
    private struct PlotSample: Identifiable {
        let id: String
        let date: Date
        let value: Double
        let segment: UInt64
    }

    private func plotSamples(_ metric: KeyPath<Metric, Double?>, scale: Double) -> [PlotSample] {
        var samples: [PlotSample] = []
        var segment: UInt64 = 0
        var previousSegment: UInt64?
        for point in points {
            if previousSegment != point.segment { segment += 1 }
            previousSegment = point.segment
            guard let value = point[keyPath: metric] else { segment += 1; continue }
            samples.append(PlotSample(id: point.id, date: point.date,
                                      value: value / scale, segment: segment))
        }
        return samples
    }

}

@MainActor
private final class ChartInspectionState: ObservableObject {
    @Published var date: Date?
}

private struct ChartInspection: ViewModifier {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme

    @ObservedObject var inspection: ChartInspectionState
    let readout: (Metric) -> String

    func body(content: Content) -> some View {
        content.chartXScale(domain: contextDomain)
            .chartXAxis { AxisMarks(values: .automatic(desiredCount: 4)) }
            .chartYAxis { AxisMarks(values: .automatic(desiredCount: 4)) }
            .chartOverlay { proxy in
                GeometryReader { geometry in
                    ZStack(alignment: .topLeading) {
                        Color.clear.contentShape(Rectangle())
                            .onTapGesture { location in
                                guard !model.busy, model.selection != nil else { return }
                                guard let frame = proxy.plotFrame else { return }
                                let plot = geometry[frame]
                                guard plot.contains(location),
                                      let date: Date = proxy.value(atX: location.x - plot.minX)
                                else { return }
                                let seconds = date.timeIntervalSince1970
                                guard seconds.isFinite, seconds >= 0,
                                      seconds < Double(UInt64.max) else { return }
                                inspection.date = nil
                                model.load(second: UInt64(seconds))
                            }
                            .onContinuousHover { phase in
                                switch phase {
                                case .active(let location):
                                    guard !model.busy, let frame = proxy.plotFrame else { return }
                                    let plot = geometry[frame]
                                    inspection.date = plot.contains(location)
                                        ? proxy.value(atX: location.x - plot.minX) : nil
                                case .ended: inspection.date = nil
                                }
                            }
                        if let date = inspection.date {
                            VStack(alignment: .leading) {
                                if let point = inspectedPoint {
                                    Text(point.date, format: .dateTime.hour().minute().second())
                                    Text(readout(point)).monospacedDigit()
                                } else {
                                    Text(date, format: .dateTime.hour().minute().second())
                                    Text("No recorded observation")
                                }
                            }.font(.caption).padding(6)
                                .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.controlRadius))
                                .allowsHitTesting(false)
                            if let point = inspectedPoint,
                               let x = proxy.position(forX: point.date), let frame = proxy.plotFrame {
                                Rectangle().fill(theme.focus.opacity(0.6)).frame(width: 1)
                                    .offset(x: geometry[frame].minX + x).allowsHitTesting(false)
                            }
                        }
                        if let selection = model.selection,
                           let x = proxy.position(forX: cursorDate(selection)),
                           let frame = proxy.plotFrame {
                            Rectangle().fill(theme.muted.opacity(0.5)).frame(width: 1)
                                .offset(x: geometry[frame].minX + x).allowsHitTesting(false)
                        }
                    }
                }
            }
            .focusable()
            .accessibilityHint("Use Left and Right to inspect observations, Home or End for the range, and Return to select the inspected second.")
            .onKeyPress(.leftArrow) { inspect(step: -1) }
            .onKeyPress(.rightArrow) { inspect(step: 1) }
            .onKeyPress(.home) { inspectBoundary(first: true) }
            .onKeyPress(.end) { inspectBoundary(first: false) }
            .onKeyPress(.return) {
                guard !model.busy, let point = inspectedPoint else { return .ignored }
                inspection.date = nil
                model.load(second: point.wallNs / 1_000_000_000)
                return .handled
            }
            .onChange(of: model.selection?.second) { _, _ in inspection.date = nil }
    }

    private var inspectedPoint: Metric? {
        guard let date = inspection.date else { return nil }
        let seconds = date.timeIntervalSince1970
        guard seconds.isFinite, seconds >= 0, seconds < Double(UInt64.max) else { return nil }
        let second = UInt64(seconds)
        if let selected = model.selection?.summary,
           selected.wallNs / 1_000_000_000 == second { return selected }
        return model.selection?.trends.first { $0.wallNs / 1_000_000_000 == second }
    }

    private func inspect(step: Int) -> KeyPress.Result {
        guard !model.busy, let points = model.selection?.trends, !points.isEmpty else { return .ignored }
        let index = inspectedPoint.flatMap { point in points.firstIndex { $0.id == point.id } }
            ?? points.count - 1
        inspection.date = points[min(points.count - 1, max(0, index + step))].date
        return .handled
    }

    private func inspectBoundary(first: Bool) -> KeyPress.Result {
        guard !model.busy, let points = model.selection?.trends else { return .ignored }
        guard let point = first ? points.first : points.last else { return .ignored }
        inspection.date = point.date
        return .handled
    }

    private func cursorDate(_ selection: Selection) -> Date {
        if let observation = selection.selected {
            return Date(timeIntervalSince1970: Double(observation.wallNs) / 1e9)
        }
        return Date(timeIntervalSince1970: Double(selection.second))
    }

    private var contextDomain: ClosedRange<Date> {
        let end = model.selection?.second ?? 0
        let first = end >= 899 ? end - 899 : 0
        return Date(timeIntervalSince1970: Double(first))...Date(timeIntervalSince1970: Double(end) + 1)
    }

}
