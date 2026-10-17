import SwiftUI

struct RecordedMemoryData {
    let selection: Selection
    let theme: FlightdeckTheme

    var selectedID: String? {
        selection.selected.map { "\($0.run):\($0.boot):\($0.publication)" }
    }

    var categories: [TimeSeriesLine] {
        [line("Active", color: theme.memoryActiveSeries, bytes: \.memoryActiveBytes),
         line("Wired", color: theme.memoryWiredSeries, bytes: \.memoryWiredBytes),
         line("Compressed", color: theme.memoryCompressedSeries, bytes: \.memoryCompressedBytes)]
    }

    var swap: TimeSeriesLine { line("Swap used", color: theme.temperatureSeries, bytes: \.swapUsedBytes) }

    var pressureText: String {
        switch selection.summary?.pressure {
        case 0: "Normal"
        case 1: "Warning"
        case 2: "Critical"
        default: "Unavailable"
        }
    }

    var domain: ClosedRange<Double> {
        let first = selection.second >= 899 ? selection.second - 899 : 0
        return Double(first)...(Double(selection.second) + 1)
    }

    func plot(lines: [TimeSeriesLine]) -> TimeSeriesModel? {
        let samples = selection.trends.map {
            TimeSeriesSample(id: $0.id,
                time: Double($0.wallNs / 1_000_000_000) + Double($0.wallNs % 1_000_000_000) / 1e9,
                segment: "\($0.run):\($0.boot):\($0.segment)")
        }
        return TimeSeriesModel(samples: samples, lines: lines, domain: domain)
    }

    private func line(_ name: String, color: Color,
                      bytes: KeyPath<Metric, UInt64?>) -> TimeSeriesLine {
        TimeSeriesLine(name: name, color: color,
            values: selection.trends.map { $0[keyPath: bytes].map { Double($0) / 1_073_741_824 } })
    }
}

@MainActor
extension RecordingModel {
    func enterMemory() { route = "Memory" }
}

struct RecordedMemory: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @State private var inspectedID: String?
    @State private var inspecting = false

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            if let selection = model.selection {
                let data = RecordedMemoryData(selection: selection, theme: theme)
                if selection.selected == nil {
                    Label("No recorded tick at this second", systemImage: "clock.badge.questionmark")
                        .foregroundStyle(theme.muted)
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)],
                          alignment: .leading, spacing: theme.fieldGap) {
                    summary("Active", bytes: selection.summary?.memoryActiveBytes)
                    summary("Wired", bytes: selection.summary?.memoryWiredBytes)
                    summary("Compressed", bytes: selection.summary?.memoryCompressedBytes)
                    summary("Swap used", bytes: selection.summary?.swapUsedBytes)
                    summary("Installed", bytes: selection.summary?.memoryTotalBytes)
                    summary("Memory pressure", reading: data.pressureText, note: "macOS verdict")
                }
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: theme.panelGap) {
                        categoryChart(data).frame(minWidth: 330)
                        swapChart(data).frame(minWidth: 330)
                    }
                    VStack(spacing: theme.panelGap) { categoryChart(data); swapChart(data) }
                }
                Text("Active, wired and compressed are the recorded VM categories. They are shown independently. Memory pressure is macOS’s verdict; compressed memory and swap can remain in use at normal pressure.")
                    .font(.callout).foregroundStyle(theme.muted)
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

    private func resetInspection() { inspectedID = nil; inspecting = false }

    private func summary(_ title: String, bytes: UInt64?) -> some View {
        summary(title, reading: dashboardReading(bytes.map { Double($0) / 1_073_741_824 },
            unit: "GiB"), note: "Recorded amount")
    }

    private func summary(_ title: String, reading: String, note: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.caption).foregroundStyle(theme.muted)
            Text(reading).font(theme.metricFont).monospacedDigit()
                .lineLimit(1).minimumScaleFactor(0.65)
            Text(note).font(.caption).foregroundStyle(theme.muted)
        }.frame(maxWidth: .infinity, alignment: .leading).padding(theme.panelPadding)
            .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
            .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
    }

    private func categoryChart(_ data: RecordedMemoryData) -> some View {
        RecordedResourceChart(title: "Recorded memory categories", data: data,
            lines: data.categories, inspectedID: $inspectedID, inspecting: $inspecting,
            explanation: "Independent active, wired and compressed VM readings from this recording. These source categories are not app memory or a computed memory-used total.")
    }

    private func swapChart(_ data: RecordedMemoryData) -> some View {
        RecordedResourceChart(title: "Swap", data: data, lines: [data.swap],
            inspectedID: $inspectedID, inspecting: $inspecting,
            explanation: "Recorded bytes held in swap at each observation. This is an amount, not swap traffic per second.")
    }
}

private struct RecordedResourceChart: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    let data: RecordedMemoryData
    let lines: [TimeSeriesLine]
    @Binding var inspectedID: String?
    @Binding var inspecting: Bool
    let explanation: String
    @FocusState private var focused: Bool

    private var readoutID: String? { inspecting ? inspectedID : data.selectedID }
    private var readoutIndex: Int? {
        data.selection.trends.firstIndex { $0.id == readoutID }
    }

    var body: some View {
        Panel(title: title, explanation: explanation) {
            if let plot = data.plot(lines: lines) {
                TimeSeriesPlot(model: plot, style: TimeSeriesStyle(unit: "GiB", fill: 0.1),
                    selectedTime: readoutID.flatMap { plot.selectedTime(id: $0) }, labels: labels)
                    .frame(height: theme.chartHeight)
                    .overlay(GeometryReader { geometry in
                        Color.clear.contentShape(Rectangle())
                            .onContinuousHover { phase in
                                switch phase {
                                case .active(let location):
                                    inspecting = true
                                    inspectedID = sample(at: location, size: geometry.size)?.id
                                case .ended: inspectedID = nil; inspecting = false
                                }
                            }
                            .onTapGesture { location in
                                if let point = sample(at: location, size: geometry.size) { select(point) }
                            }
                    })
                    .focusable().focused($focused)
                    .overlay(RoundedRectangle(cornerRadius: theme.controlRadius)
                        .stroke(focused ? theme.focus : .clear, lineWidth: 2))
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(title + ", GiB")
                    .accessibilityValue(lines.map { $0.name + " " + reading($0) }.joined(separator: "; "))
                    .accessibilityHint("Left and Right inspect observations. Home and End inspect range bounds. Return selects the inspected second.")
                    .onKeyPress(.leftArrow) { inspect(step: -1) }
                    .onKeyPress(.rightArrow) { inspect(step: 1) }
                    .onKeyPress(.home) { inspectBoundary(first: true) }
                    .onKeyPress(.end) { inspectBoundary(first: false) }
                    .onKeyPress(.escape) {
                        inspectedID = nil; inspecting = false; return .handled
                    }
                    .onKeyPress(.return) {
                        guard let index = readoutIndex else { return .ignored }

                        select(data.selection.trends[index])
                        return .handled
                    }
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    HStack(spacing: 6) {
                        RoundedRectangle(cornerRadius: 1).fill(line.color)
                            .frame(width: 12, height: 3)
                        Text(line.name)
                        Spacer()
                        Text(reading(line)).monospacedDigit().foregroundStyle(theme.muted)
                    }.font(.caption)
                }
                if let index = readoutIndex {
                    Text(data.selection.trends[index].date,
                         format: .dateTime.hour().minute().second())
                        .font(.caption).foregroundStyle(theme.muted)
                }
            } else {
                Text("Chart readings exceed the display bounds.").foregroundStyle(theme.muted)
            }
        }
    }

    private var labels: [String] {
        (0...2).map { offset in
            Date(timeIntervalSince1970: data.domain.lowerBound
                + (data.domain.upperBound - data.domain.lowerBound) * Double(offset) / 2)
                .formatted(.dateTime.hour().minute())
        }
    }

    private func reading(_ line: TimeSeriesLine) -> String {
        dashboardReading(readoutIndex.flatMap { line.values[$0] }, unit: "GiB")
    }

    private func sample(at location: CGPoint, size: CGSize) -> Metric? {
        guard !model.busy else { return nil }

        let plot = TimeSeriesPlot.frame(size: size)
        guard plot.contains(location) else { return nil }

        let time = data.domain.lowerBound + (location.x - plot.minX) / plot.width
            * (data.domain.upperBound - data.domain.lowerBound)
        let candidates = data.selection.trends.filter {
            Double($0.wallNs / 1_000_000_000) == floor(time)
        }
        return candidates.first { $0.id == data.selectedID } ?? candidates.first
    }

    private func inspect(step: Int) -> KeyPress.Result {
        guard !model.busy, !data.selection.trends.isEmpty else { return .ignored }

        let index = readoutIndex ?? data.selection.trends.count - 1
        inspecting = true
        inspectedID = data.selection.trends[min(data.selection.trends.count - 1,
            max(0, index + step))].id
        return .handled
    }

    private func inspectBoundary(first: Bool) -> KeyPress.Result {
        guard !model.busy, let point = first ? data.selection.trends.first
            : data.selection.trends.last else { return .ignored }

        inspecting = true
        inspectedID = point.id
        return .handled
    }

    private func select(_ point: Metric) {
        guard !model.busy else { return }

        let second = point.wallNs / 1_000_000_000
        let alternatives = data.selection.trends.filter { $0.wallNs / 1_000_000_000 == second }
        guard let index = alternatives.firstIndex(where: { $0.id == point.id }),
              let alternative = UInt32(exactly: index) else { return }

        inspectedID = nil
        inspecting = false
        model.load(second: second, alternative: alternative)
    }
}
