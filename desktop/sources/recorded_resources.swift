import SwiftUI

struct RecordedChartContext {
    let selection: Selection

    var selectedID: String? {
        selection.selected.map { "\($0.run):\($0.boot):\($0.publication)" }
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

    func point(at time: Double) -> Metric? {
        guard time.isFinite else { return nil }

        let candidates = selection.trends.filter {
            Double($0.wallNs / 1_000_000_000) == floor(time)
        }
        return candidates.first { $0.id == selectedID } ?? candidates.first
    }

    func target(id: String) -> RecordedSelectionTarget? {
        guard let point = selection.trends.first(where: { $0.id == id }) else { return nil }

        return RecordedSelectionTarget(second: point.wallNs / 1_000_000_000,
            identity: point.identity)
    }
}

struct RecordedSelectionTarget: Equatable {
    let second: UInt64
    let identity: RecordedObservationIdentity
}

struct RecordedMemoryData {
    let selection: Selection
    let theme: FlightdeckTheme

    var context: RecordedChartContext { RecordedChartContext(selection: selection) }
    var selectedID: String? { context.selectedID }

    var categories: [TimeSeriesLine] {
        [line("Active", color: theme.memoryActiveSeries, bytes: \.memoryActiveBytes),
         line("Wired", color: theme.memoryWiredSeries, bytes: \.memoryWiredBytes),
         line("Compressed", color: theme.memoryCompressedSeries, bytes: \.memoryCompressedBytes)]
    }

    var swap: TimeSeriesLine { line("Used", color: theme.swapUsedSeries, bytes: \.swapUsedBytes) }

    var swapAllocated: TimeSeriesLine {
        var allocated = line("Allocated", color: theme.swapAllocatedSeries, bytes: \.swapTotalBytes)

        allocated.strokePattern = .dashed
        allocated.fill = 0

        return allocated
    }

    var swapTraffic: [TimeSeriesLine] {
        [TimeSeriesLine(name: "Read from swap", color: theme.swapReadSeries,
            values: selection.trends.map { $0.swapInBytesPerSecond.map { $0 / 1_000_000 } }),
         TimeSeriesLine(name: "Write to swap", color: theme.swapWriteSeries,
            values: selection.trends.map { $0.swapOutBytesPerSecond.map { $0 / 1_000_000 } },
            negative: true)]
    }

    var paging: [TimeSeriesLine] {
        [rate("Page-ins", color: theme.memoryPageinsSeries, pattern: .solid,
              reading: \.memoryPageinsPerSecond),
         rate("Compressed", color: theme.memoryCompressionsSeries,
              pattern: .dashed, reading: \.memoryCompressionsPerSecond),
         rate("Decompressed", color: theme.memoryDecompressionsSeries,
              pattern: .dotted, reading: \.memoryDecompressionsPerSecond)]
    }

    var pressureText: String {
        selection.summary?.pressure.flatMap { MemoryPressureState(rawValue: $0) }?.label
            ?? "Unavailable"
    }

    var domain: ClosedRange<Double> { context.domain }

    func plot(lines: [TimeSeriesLine]) -> TimeSeriesModel? { context.plot(lines: lines) }

    private func line(_ name: String, color: Color,
                      bytes: KeyPath<Metric, UInt64?>) -> TimeSeriesLine {
        TimeSeriesLine(name: name, color: color,
            values: selection.trends.map { $0[keyPath: bytes].map { Double($0) / 1_073_741_824 } })
    }

    private func rate(_ name: String, color: Color,
                      pattern: TimeSeriesStrokePattern,
                      reading: KeyPath<Metric, Double?>) -> TimeSeriesLine {
        TimeSeriesLine(name: name, color: color,
            values: selection.trends.map { $0[keyPath: reading] }, strokePattern: pattern)
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
                        pressureChart(data).frame(minWidth: 330)
                    }
                    VStack(spacing: theme.panelGap) { categoryChart(data); pressureChart(data) }
                }
                Self.swapTrafficChart(data, inspectedID: $inspectedID, inspecting: $inspecting)
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: theme.panelGap) {
                        swapChart(data).frame(minWidth: 330)
                        pagingChart(data).frame(minWidth: 330)
                    }
                    VStack(spacing: theme.panelGap) { swapChart(data); pagingChart(data) }
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
        RecordedResourceChart(title: "Recorded memory categories", context: data.context,
            lines: data.categories, style: TimeSeriesStyle(unit: "GiB", fill: 0.1), inspectedID: $inspectedID, inspecting: $inspecting,
            explanation: "Independent active, wired and compressed VM readings from this recording. These source categories are not app memory or a computed memory-used total.")
    }

    private func pressureChart(_ data: RecordedMemoryData) -> some View {
        RecordedMemoryPressure(selection: data.selection, inspectedID: $inspectedID,
            inspecting: $inspecting, busy: model.busy, onSelect: model.select)
    }

    private func swapChart(_ data: RecordedMemoryData) -> some View {
        RecordedResourceChart(title: "Swap", context: data.context,
            lines: [data.swap, data.swapAllocated],
            style: TimeSeriesStyle(unit: "GiB", fill: 0.1),
            inspectedID: $inspectedID, inspecting: $inspecting,
            explanation: "Used is the amount held in swap. Allocated is the total swap space macOS has allocated. Both are amounts in GiB, not traffic rates.")
    }

    static func swapTrafficChart(_ data: RecordedMemoryData,
                                 inspectedID: Binding<String?>,
                                 inspecting: Binding<Bool>) -> RecordedResourceChart {
        RecordedResourceChart(title: "Swap traffic", context: data.context,
            lines: data.swapTraffic, style: TimeSeriesStyle(unit: "MB/s"),
            inspectedID: inspectedID, inspecting: inspecting,
            explanation: "Read from swap and write to swap in decimal MB/s over the measured interval. Writes mirror below zero; both readouts stay nonnegative. Missing or reset intervals remain gaps.")
    }

    private func pagingChart(_ data: RecordedMemoryData) -> some View {
        RecordedResourceChart(title: "Page-ins and compression", context: data.context,
            lines: data.paging, style: TimeSeriesStyle(unit: "/s"),
            inspectedID: $inspectedID, inspecting: $inspecting,
            explanation: "Page-ins, compressions and decompressions per second over the measured interval. Unavailable intervals remain gaps. These are page counts, not bytes per second. Paged-out byte rates are unavailable: the source supplies page counts without a recorded page size for conversion.")
    }
}

struct RecordedResourceChart: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    let context: RecordedChartContext
    let lines: [TimeSeriesLine]
    let style: TimeSeriesStyle
    @Binding var inspectedID: String?
    @Binding var inspecting: Bool
    let explanation: String
    @FocusState private var focused: Bool

    private var readoutID: String? { inspecting ? inspectedID : context.selectedID }
    private var readoutIndex: Int? {
        context.selection.trends.firstIndex { $0.id == readoutID }
    }

    var body: some View {
        Panel(title: title, explanation: explanation) {
            if let plot = context.plot(lines: lines) {
                TimeSeriesPlot(model: plot, style: style,
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
                    .accessibilityLabel(title + ", " + style.unit)
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

                        select(context.selection.trends[index])
                        return .handled
                    }
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    HStack(spacing: 6) {
                        Path { path in
                            path.move(to: CGPoint(x: 0, y: 3))
                            path.addLine(to: CGPoint(x: 32, y: 3))
                        }.stroke(line.color, style: line.strokePattern.style(width: theme.chartStroke))
                            .frame(width: 32, height: 6).accessibilityHidden(true)
                        Text(line.name)
                        Spacer()
                        Text(reading(line)).monospacedDigit().foregroundStyle(theme.muted)
                    }.font(.caption)
                }
                if let index = readoutIndex {
                    Text(context.selection.trends[index].date,
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
            Date(timeIntervalSince1970: context.domain.lowerBound
                + (context.domain.upperBound - context.domain.lowerBound) * Double(offset) / 2)
                .formatted(.dateTime.hour().minute())
        }
    }

    func reading(_ line: TimeSeriesLine) -> String {
        dashboardReading(readoutIndex.flatMap { line.values[$0] }, unit: line.unit ?? style.unit)
    }

    private func sample(at location: CGPoint, size: CGSize) -> Metric? {
        guard !model.busy else { return nil }

        let plot = TimeSeriesPlot.frame(size: size)
        guard plot.contains(location) else { return nil }

        let time = context.domain.lowerBound + (location.x - plot.minX) / plot.width
            * (context.domain.upperBound - context.domain.lowerBound)
        return context.point(at: time)
    }

    private func inspect(step: Int) -> KeyPress.Result {
        guard !model.busy, !context.selection.trends.isEmpty else { return .ignored }

        let index = readoutIndex ?? context.selection.trends.count - 1
        inspecting = true
        inspectedID = context.selection.trends[min(context.selection.trends.count - 1,
            max(0, index + step))].id
        return .handled
    }

    private func inspectBoundary(first: Bool) -> KeyPress.Result {
        guard !model.busy, let point = first ? context.selection.trends.first
            : context.selection.trends.last else { return .ignored }

        inspecting = true
        inspectedID = point.id
        return .handled
    }

    private func select(_ point: Metric) {
        guard !model.busy else { return }

        guard let target = context.target(id: point.id) else { return }

        inspectedID = nil
        inspecting = false
        model.select(target)
    }
}
