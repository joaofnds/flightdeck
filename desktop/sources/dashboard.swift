import SwiftUI

struct Dashboard: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @StateObject private var inspection = ChartInspectionState()
    @StateObject private var schematicPreview = SchematicPreviewState()

    private var points: [Metric] { model.selection?.trends ?? [] }
    private var summary: Metric? { model.selection?.summary }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            if model.selection?.selected == nil, !model.busy {
                Label("No recorded tick at this second", systemImage: "clock.badge.questionmark")
                    .foregroundStyle(theme.muted)
            }
            summaryMetrics
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 460), alignment: .leading)],
                      alignment: .leading, spacing: theme.panelGap) {
                linePanel("CPU activity", value: summary?.cpuPercent, unit: "%", color: theme.cpuSeries,
                          explanation: "Busy ticks across all recorded logical CPUs divided by total CPU ticks. Requires consecutive compatible observations.",
                          domain: 0...100, metric: \.cpuPercent)
                memoryPanel
                gpuActivityPanel
                networkPanel
                linePanel("System power", value: summary?.powerWatts, unit: "W", color: theme.systemPowerSeries,
                          explanation: "Recorded SMC system-power reading. Unread source remains unavailable.",
                          metric: \.powerWatts)
                linePanel("SoC temperature", value: summary?.socCelsius, unit: "°C", color: theme.temperatureSeries,
                          explanation: "Recorded SoC sensor reading. This does not identify separate CPU/GPU sensor positions.",
                          metric: \.socCelsius)
            }
            VStack(alignment: .leading, spacing: theme.fieldGap) {
                Text("Inside your Mac").font(.title3.weight(.semibold))
                MacBookSchematic(readings: schematicReadings(selection: model.selection,
                    fans: model.fans), preview: schematicPreview)
                    .help("Illustration: 16-inch MacBook Pro. Fan channel sides are unverified.")
            }
        }
        .onChange(of: model.schematicPreviewSource, initial: true) { _, source in
            schematicPreview.observeSource(source)
        }
    }

    private var summaryMetrics: some View {
        MetricSummaryGrid {
            MetricSummary(title: "CPU", reading: dashboardReading(summary?.cpuPercent, unit: "%"),
                note: "All logical CPUs", color: theme.cpuSeries, action: model.enterProcesses)
            MetricSummary(title: "GPU", reading: dashboardReading(summary?.gpuActivePercent, unit: "%"),
                note: "Active time", color: theme.gpuActivitySeries,
                action: { model.navigate(to: "GPU") })
            MetricSummary(title: "Memory pressure", reading: summary?.pressure.flatMap {
                MemoryPressureState(rawValue: $0)?.label
            } ?? "Unavailable", note: "macOS verdict", action: { model.navigate(to: "Memory") })
            MetricSummary(title: "System power", reading: dashboardReading(summary?.powerWatts, unit: "W"),
                note: "Whole-machine sensor", color: theme.systemPowerSeries,
                action: { model.navigate(to: "Energy") })
            MetricSummary(title: "Receive", reading: dashboardReading(summary?.networkRxBps.map {
                $0 / 1_000_000
            }, unit: "MB/s"), note: "External interfaces", color: theme.networkReceivedSeries,
                action: { model.navigate(to: "Network") })
        }
    }

    private var networkPanel: some View {
        Panel(title: "Network traffic", explanation: "Rates across recorded non-loopback interfaces. Sent traffic is mirrored below zero. VPN and tunnel traffic can be counted more than once.") {
            if let selection = model.selection {
                let data = RecordedComponentData(context: RecordedChartContext(selection: selection),
                    theme: theme)
                plot(context: data.context, lines: data.network,
                    style: TimeSeriesStyle(unit: "KiB/s", fill: 0.1)) { point in
                    "Received " + dashboardReading(point.networkRxBps.map { $0 / 1024 }, unit: "KiB/s")
                        + " · Sent " + dashboardReading(point.networkTxBps.map { $0 / 1024 }, unit: "KiB/s")
                }
                HStack(spacing: theme.panelGap) {
                    Label("Received", systemImage: "arrow.down").foregroundStyle(theme.networkReceivedSeries)
                    Spacer()
                    Text(dashboardReading(summary?.networkRxBps.map { $0 / 1024 }, unit: "KiB/s"))
                    Label("Sent", systemImage: "arrow.up").foregroundStyle(theme.networkSentSeries)
                    Text(dashboardReading(summary?.networkTxBps.map { $0 / 1024 }, unit: "KiB/s"))
                }.font(.caption).monospacedDigit()
            }
        }
    }

    private var gpuActivityPanel: some View {
        Panel(title: "GPU activity",
              explanation: "Time in recorded active GPU states divided by the measured continuous interval. Requires compatible observations and complete matching active states.") {
            if let selection = model.selection {
                let data = RecordedComponentData(context: RecordedChartContext(selection: selection),
                    theme: theme)
                plot(context: data.context, lines: [data.gpuActivity],
                    style: TimeSeriesStyle(unit: "%", minimum: 0,
                        maximum: data.gpuActivityStyle.maximum, fill: 0,
                        interpolation: .linear)) { point in
                    metricText(point.gpuActivePercent, unit: "%")
                }
            }
            chartReading("Active time", value: summary?.gpuActivePercent,
                unit: "%", color: theme.gpuActivitySeries)
        }
    }

    private var memoryPanel: some View {
        Panel(title: "Memory", explanation: "Recorded active, wired and compressed memory in GiB. These are source categories; cached/free memory remains available in the recorded-memory table.") {
            if let selection = model.selection {
                let memory = RecordedMemoryData(selection: selection, theme: theme)
                plot(context: memory.context, lines: memory.categories,
                     style: TimeSeriesStyle(unit: "GiB", stack: true, gradient: false,
                         fill: 1, interpolation: .linear)) { point in
                    metricText(point.memoryActiveBytes.map { Double($0) / 1_073_741_824 },
                               unit: "GiB active")
                }
                HStack(spacing: theme.fieldGap) {
                    ForEach(Array(memory.categories.enumerated()), id: \.offset) { _, line in
                        HStack(spacing: 6) {
                            Circle().fill(line.color).frame(width: 6, height: 6)
                            Text(line.name).foregroundStyle(theme.muted)
                        }
                    }
                    Spacer(minLength: 0)
                    Text(dashboardReading(summary?.memoryTotalBytes.map {
                        Double($0) / 1_073_741_824
                    }, unit: "GiB installed"))
                }.font(.caption)
            }
        }
    }

    private func linePanel(_ title: String, value: Double?, unit: String, color: Color,
                           explanation: String, domain: ClosedRange<Double>? = nil,
                           metric: KeyPath<Metric, Double?>, scale: Double = 1) -> some View {
        Panel(title: title, explanation: explanation) {
            if let selection = model.selection {
                let line = TimeSeriesLine(name: title, color: color,
                    values: points.map { $0[keyPath: metric].map { $0 / scale } })
                let upper = domain?.upperBound ?? max(1,
                    line.values.compactMap { $0 }.max().map { $0 * 1.1 } ?? 1)
                plot(context: RecordedChartContext(selection: selection), lines: [line],
                     style: TimeSeriesStyle(unit: unit, minimum: domain?.lowerBound ?? 0,
                         maximum: upper, fill: 0, interpolation: .linear)) { point in
                    metricText(point[keyPath: metric].map { $0 / scale }, unit: unit)
                }
            }
            chartReading(title, value: value, unit: unit, color: color)
        }
    }

    private func chartReading(_ title: String, value: Double?, unit: String,
                              color: Color) -> some View {
        HStack(spacing: 6) {
            Circle().fill(color).frame(width: 6, height: 6).accessibilityHidden(true)
            Text(title).foregroundStyle(theme.muted)
            Spacer()
            Text(dashboardReading(value, unit: unit)).monospacedDigit()
        }.font(.caption)
    }

    @ViewBuilder
    private func plot(context: RecordedChartContext, lines: [TimeSeriesLine],
                      style: TimeSeriesStyle, readout: @escaping (Metric) -> String) -> some View {
        if let plot = context.plot(lines: lines) {
            let labels = (0...2).map { offset in
                Date(timeIntervalSince1970: context.domain.lowerBound
                    + (context.domain.upperBound - context.domain.lowerBound) * Double(offset) / 2)
                    .formatted(.dateTime.hour().minute())
            }
            let selectedTime = context.selection.selected.map { Double($0.wallNs) / 1e9 }
                ?? Double(context.selection.second)
            TimeSeriesPlot(model: plot, style: style, selectedTime: selectedTime, labels: labels)
                .frame(height: theme.chartHeight)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(lines.map(\.name).joined(separator: ", "))
                .accessibilityValue(inspection.reading(in: context.selection, readout: readout))
                .modifier(ChartInspection(inspection: inspection, domain: context.domain,
                                          readout: readout))
        } else {
            Text("Chart readings exceed the display bounds.").foregroundStyle(theme.muted)
        }
    }
}

@MainActor
final class ChartInspectionState: ObservableObject {
    @Published private var value: Position?

    private enum Position {
        case pointer(Date)
        case observation(String)
    }

    func date(in selection: Selection?) -> Date? {
        if case .some(.pointer(let date)) = value { return date }

        return point(in: selection)?.date
    }

    func point(in selection: Selection?) -> Metric? {
        guard let value, let selection else { return nil }

        switch value {
        case .observation(let id):
            return selection.trends.first { $0.id == id }
        case .pointer(let date):
            let seconds = date.timeIntervalSince1970
            guard seconds.isFinite, seconds >= 0, seconds < Double(UInt64.max) else {
                return nil
            }

            let second = UInt64(seconds)
            if let selected = selection.summary, selected.wallNs / 1_000_000_000 == second {
                return selected
            }

            return selection.trends.first { $0.wallNs / 1_000_000_000 == second }
        }
    }

    func target(in selection: Selection?) -> RecordedSelectionTarget? {
        point(in: selection).map {
            RecordedSelectionTarget(second: $0.wallNs / 1_000_000_000, identity: $0.identity)
        }
    }

    func reading(in selection: Selection?, readout: (Metric) -> String) -> String {
        guard let date = date(in: selection) else {
            return selection?.summary.map(readout) ?? "Unavailable"
        }

        let reading = point(in: selection).map(readout) ?? "No recorded observation"
        return date.formatted(.dateTime.hour().minute().second()) + ", " + reading
    }

    func hover(at date: Date?) { value = date.map(Position.pointer) }
    func clear() { value = nil }

    func move(step: Int, in selection: Selection?) -> Bool {
        guard let points = selection?.trends, !points.isEmpty else { return false }

        let index = point(in: selection).flatMap { point in
            points.firstIndex { $0.id == point.id }
        } ?? points.count - 1
        value = .observation(points[min(points.count - 1, max(0, index + step))].id)
        return true
    }

    func boundary(first: Bool, in selection: Selection?) -> Bool {
        guard let point = first ? selection?.trends.first : selection?.trends.last else {
            return false
        }

        value = .observation(point.id)
        return true
    }
}

private struct ChartInspection: ViewModifier {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme

    @ObservedObject var inspection: ChartInspectionState
    let domain: ClosedRange<Double>
    let readout: (Metric) -> String
    @FocusState private var focused: Bool

    func body(content: Content) -> some View {
        content.overlay {
            GeometryReader { geometry in
                let coordinates = TimeSeriesCoordinates(domain: domain, size: geometry.size)
                ZStack(alignment: .topLeading) {
                    Color.clear.contentShape(Rectangle())
                        .onTapGesture { location in
                            focused = true
                            guard !model.busy, let selection = model.selection else { return }

                            guard let seconds = coordinates.time(at: location) else { return }
                            guard seconds.isFinite, seconds >= 0,
                                  seconds < Double(UInt64.max) else { return }
                            inspection.clear()
                            let context = RecordedChartContext(selection: selection)
                            if let point = context.point(at: seconds),
                               let target = context.target(id: point.id) {
                                model.select(target)
                            } else {
                                model.load(second: UInt64(seconds))
                            }
                        }
                        .onContinuousHover { phase in
                            switch phase {
                            case .active(let location):
                                guard !model.busy else { return }
                                inspection.hover(at: coordinates.time(at: location).map {
                                    Date(timeIntervalSince1970: $0)
                                })
                            case .ended: inspection.clear()
                            }
                        }
                    if let date = inspection.date(in: model.selection) {
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
                           let x = coordinates.position(at: point.date.timeIntervalSince1970) {
                            Rectangle().fill(theme.focus.opacity(0.6))
                                .frame(width: 1, height: coordinates.frame.height)
                                .offset(x: x, y: coordinates.frame.minY).allowsHitTesting(false)
                        }
                    }
                }
            }
        }
            .focusable().focused($focused)
            .overlay(RoundedRectangle(cornerRadius: theme.controlRadius)
                .stroke(focused ? theme.focus : .clear, lineWidth: 2))
            .accessibilityHint("Use Left and Right to inspect observations, Home or End for the range, and Return to select the inspected second.")
            .onKeyPress(.leftArrow) { inspect(step: -1) }
            .onKeyPress(.rightArrow) { inspect(step: 1) }
            .onKeyPress(.home) { inspectBoundary(first: true) }
            .onKeyPress(.end) { inspectBoundary(first: false) }
            .onKeyPress(.return) {
                guard !model.busy, let target = inspection.target(in: model.selection) else {
                    return .ignored
                }

                inspection.clear()
                model.select(target)
                return .handled
            }
            .onChange(of: model.selection?.second) { _, _ in inspection.clear() }
    }

    private var inspectedPoint: Metric? {
        inspection.point(in: model.selection)
    }

    private func inspect(step: Int) -> KeyPress.Result {
        guard !model.busy else { return .ignored }

        return inspection.move(step: step, in: model.selection) ? .handled : .ignored
    }

    private func inspectBoundary(first: Bool) -> KeyPress.Result {
        guard !model.busy else { return .ignored }

        return inspection.boundary(first: first, in: model.selection) ? .handled : .ignored
    }
}
