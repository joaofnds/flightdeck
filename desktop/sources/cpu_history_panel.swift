import SwiftUI

struct LogicalCPUHistoryPanel: View {
    @Environment(\.flightdeckTheme) private var theme
    let history: SelectedCPUHistory?
    let notice: String?
    let busy: Bool
    let onPage: (UInt32) -> Void
    let onSelect: (RecordedSelectionTarget) -> Void

    var body: some View {
        Panel(title: "Logical CPU history", explanation: "Busy scheduler ticks for each logical CPU over the last 15 minutes. Each reading covers its actual consecutive observation interval. Blank intervals are unavailable, including collection gaps. Logical CPU indices do not identify physical cores or clusters.") {
            if let notice {
                Text(notice).font(theme.chartStateFont).foregroundStyle(theme.warning)
            }
            if let history, !history.cpus.isEmpty {
                let geometry = CPUHistoryGeometry(history: history)

                pageControls(geometry)
                CPUHistoryPlot(geometry: geometry, busy: busy, onSelect: onSelect)
                legend
                if history.retentionLimited {
                    Text("Some CPU history is unavailable in this 15-minute window.")
                        .font(theme.compactReadingFont).foregroundStyle(theme.muted)
                }
            } else {
                Text(busy ? "Reading logical CPU history…" : "Logical CPU history unavailable.")
                    .font(theme.chartStateFont).foregroundStyle(theme.muted)
            }
        }
    }

    private func pageControls(_ geometry: CPUHistoryGeometry) -> some View {
        HStack(spacing: theme.fieldGap) {
            Text("15-minute context · Logical CPUs \(geometry.history.offset)–\(geometry.history.offset + UInt32(geometry.history.cpus.count) - 1) of \(geometry.history.total)")
                .font(theme.compactReadingFont).foregroundStyle(theme.muted)
            Spacer()
            Button("Previous") {
                if let offset = geometry.previousPage { onPage(offset) }
            }.disabled(busy || geometry.previousPage == nil)
                .accessibilityLabel("Previous logical CPU history page")
            Button("Next") {
                if let offset = geometry.nextPage { onPage(offset) }
            }.disabled(busy || geometry.nextPage == nil)
                .accessibilityLabel("Next logical CPU history page")
        }
    }

    private var legend: some View {
        HStack(spacing: theme.fieldGap) {
            legendItem("0–59%", color: theme.cpuActivityLow)
            legendItem("60–79%", color: theme.cpuActivityModerate)
            legendItem("80–89%", color: theme.cpuActivityHigh)
            legendItem("90–100%", color: theme.cpuActivityPeak)
            Text("Blank: unavailable").foregroundStyle(theme.muted)
            Spacer(minLength: 0)
        }.font(theme.compactReadingFont)
    }

    private func legendItem(_ title: String, color: Color) -> some View {
        HStack(spacing: theme.fieldGap / 2) {
            Rectangle().fill(color).frame(width: 9, height: 9).accessibilityHidden(true)
            Text(title).foregroundStyle(theme.muted)
        }
    }
}

private struct CPUHistoryPlot: View {
    @Environment(\.flightdeckTheme) private var theme
    let geometry: CPUHistoryGeometry
    let busy: Bool
    let onSelect: (RecordedSelectionTarget) -> Void
    @State private var inspected: CPUHistoryInspection?
    @State private var inspecting = false
    @FocusState private var focused: Bool

    private var readout: CPUHistoryInspection? {
        guard let candidate = inspecting ? inspected : geometry.selected,
              geometry.target(candidate) != nil else { return nil }

        return candidate
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            Canvas { context, size in draw(context: &context, size: size) }
                .frame(height: CPUHistoryPlotLayout.height(rows: geometry.history.cpus.count))
                .overlay(GeometryReader { proxy in
                    Color.clear.contentShape(Rectangle())
                        .onContinuousHover { phase in
                            switch phase {
                            case .active(let point): inspect(at: point, size: proxy.size)
                            case .ended:
                                if !focused { resetInspection() }
                            }
                        }
                        .onTapGesture { point in
                            inspect(at: point, size: proxy.size)
                            focused = true
                            selectInspected()
                        }
                })
                .focusable().focused($focused)
                .overlay(RoundedRectangle(cornerRadius: theme.controlRadius)
                    .stroke(focused ? theme.focus : .clear, lineWidth: 2))
                .onKeyPress(.leftArrow) { step(observations: -1, rows: 0) }
                .onKeyPress(.rightArrow) { step(observations: 1, rows: 0) }
                .onKeyPress(.upArrow) { step(observations: 0, rows: -1) }
                .onKeyPress(.downArrow) { step(observations: 0, rows: 1) }
                .onKeyPress(.home) { boundary(first: true) }
                .onKeyPress(.end) { boundary(first: false) }
                .onKeyPress(.escape) { resetInspection(); return .handled }
                .onKeyPress(.return) { selectInspected(); return .handled }
                .accessibilityLabel("Logical CPU history")
                .accessibilityValue(readoutText)
                .accessibilityHint("Left and Right inspect observations. Up and Down inspect logical CPUs. Return selects the exact inspected observation. Blank intervals are unavailable.")
                .accessibilityChildren {
                    ForEach(Array(geometry.history.cpus.enumerated()), id: \.element.cpu) { row, cpu in
                        Text("Logical CPU \(cpu.cpu)")
                            .accessibilityIdentifier("cpu-history-row-\(cpu.cpu)")
                            .accessibilityValue(rowReading(row))
                    }
                }
            HStack {
                Text(readoutText).lineLimit(2)
                Spacer(minLength: 0)
                if busy { Text("Reading…") }
            }.font(theme.compactReadingFont).foregroundStyle(theme.muted)
        }
        .onChange(of: geometry.history.observation.identity) { resetInspection() }
        .onChange(of: geometry.history.offset) { resetInspection() }
        .onChange(of: geometry.history.observations.count) { resetInspection() }
    }

    private var readoutText: String {
        guard let readout, let target = geometry.target(readout) else {
            return "No measurement interval here."
        }

        let point = geometry.history.observations[readout.observationIndex].observation
        let time = Date(timeIntervalSince1970: Double(target.second)
            + Double(point.wallNs % 1_000_000_000) / 1e9)
            .formatted(.dateTime.hour().minute().second().secondFraction(.fractional(3)))
        let mode = inspecting ? "Inspecting" : "Selected"

        return "\(mode) logical CPU \(geometry.history.cpus[readout.row].cpu) · \(geometry.reading(readout)) · \(time)"
    }

    private func rowReading(_ row: Int) -> String {
        guard let readout else { return "Unavailable" }

        return geometry.reading(CPUHistoryInspection(row: row,
            observationIndex: readout.observationIndex))
    }

    private func inspect(at point: CGPoint, size: CGSize) {
        guard !busy else { return }

        let plot = CPUHistoryPlotLayout.frame(size: size, rows: geometry.history.cpus.count)
        inspecting = true
        inspected = nil

        if plot.contains(point) {
            let row = Int((point.y - plot.minY) / CPUHistoryPlotLayout.rowHeight)
            inspected = geometry.inspect(row: row, fraction: (point.x - plot.minX) / plot.width)
        }
    }

    private func step(observations: Int, rows: Int) -> KeyPress.Result {
        guard !busy else { return .ignored }

        inspected = geometry.step(from: readout, observations: observations, rows: rows)
        inspecting = true

        return .handled
    }

    private func boundary(first: Bool) -> KeyPress.Result {
        guard !busy else { return .ignored }

        inspected = CPUHistoryInspection(row: readout?.row ?? 0,
            observationIndex: first ? 0 : geometry.history.observations.count - 1)
        inspecting = true

        return .handled
    }

    private func selectInspected() {
        guard !busy, let readout, let target = geometry.target(readout) else { return }

        onSelect(target)
    }

    private func resetInspection() {
        inspected = nil
        inspecting = false
    }

    private func draw(context: inout GraphicsContext, size: CGSize) {
        let plot = CPUHistoryPlotLayout.frame(size: size, rows: geometry.history.cpus.count)
        guard plot.width > 0 else { return }

        drawRows(context: &context, plot: plot)
        drawIntervals(context: &context, plot: plot)
        drawAxis(context: &context, plot: plot)
        drawMarker(context: &context, fraction: geometry.selectedFraction, plot: plot,
            color: theme.text, dashed: false)
        if inspecting, let readout {
            let point = geometry.history.observations[readout.observationIndex].observation
            drawMarker(context: &context, fraction: geometry.fraction(at: point.wallNs),
                plot: plot, color: theme.focus, dashed: true)
        }
    }

    private func drawRows(context: inout GraphicsContext, plot: CGRect) {
        for (row, cpu) in geometry.history.cpus.enumerated() {
            let top = plot.minY + Double(row) * CPUHistoryPlotLayout.rowHeight
            let rectangle = CGRect(x: plot.minX, y: top, width: plot.width,
                height: CPUHistoryPlotLayout.rowHeight - 2)
            context.fill(Path(rectangle), with: .color(theme.background))
            context.draw(Text("CPU \(cpu.cpu)").font(theme.chartAxisFont)
                .foregroundStyle(theme.muted),
                at: CGPoint(x: plot.minX - 6, y: rectangle.midY), anchor: .trailing)
        }
    }

    private func drawIntervals(context: inout GraphicsContext, plot: CGRect) {
        let palette = CPUHistoryPalette.colors(theme: theme)
        var paths = Array(repeating: Path(), count: palette.count)

        for (row, cpu) in geometry.history.cpus.enumerated() {
            for interval in geometry.intervals {
                guard let value = cpu.busyPercent[interval.observationIndex] else { continue }

                let first = geometry.fraction(at: interval.firstNs)
                let end = geometry.fraction(at: interval.endNs)
                let rectangle = CGRect(x: plot.minX + first * plot.width,
                    y: plot.minY + Double(row) * CPUHistoryPlotLayout.rowHeight,
                    width: (end - first) * plot.width,
                    height: CPUHistoryPlotLayout.rowHeight - 2)
                paths[CPUHistoryPalette.bin(value)].addRect(rectangle)
            }
        }

        for index in palette.indices {
            context.fill(paths[index], with: .color(palette[index]))
        }
    }

    private func drawAxis(context: inout GraphicsContext, plot: CGRect) {
        for tick in 0...2 {
            let fraction = Double(tick) / 2
            let nanoseconds = geometry.history.firstNs
                + (geometry.history.endNs - geometry.history.firstNs) * UInt64(tick) / 2
            let label = Date(timeIntervalSince1970: Double(nanoseconds / 1_000_000_000))
                .formatted(.dateTime.hour().minute().second())
            let anchor: UnitPoint = tick == 0 ? .leading : (tick == 2 ? .trailing : .center)

            context.draw(Text(label).font(theme.chartAxisFont).foregroundStyle(theme.muted),
                at: CGPoint(x: plot.minX + fraction * plot.width, y: plot.maxY + 13),
                anchor: anchor)
        }
    }

    private func drawMarker(context: inout GraphicsContext, fraction: Double,
                            plot: CGRect, color: Color, dashed: Bool) {
        let x = plot.minX + fraction * plot.width
        var path = Path()
        path.move(to: CGPoint(x: x, y: plot.minY))
        path.addLine(to: CGPoint(x: x, y: plot.maxY))
        context.stroke(path, with: .color(color),
            style: StrokeStyle(lineWidth: 1, dash: dashed ? [3, 3] : []))
    }
}

private enum CPUHistoryPlotLayout {
    static let rowHeight: CGFloat = 18

    static func height(rows: Int) -> CGFloat { Double(rows) * rowHeight + 28 }

    static func frame(size: CGSize, rows: Int) -> CGRect {
        CGRect(x: 52, y: 2, width: max(0, size.width - 56), height: Double(rows) * rowHeight)
    }
}

private enum CPUHistoryPalette {
    static func bin(_ value: Double) -> Int { min(20, max(0, Int(value / 5))) }

    static func colors(theme: FlightdeckTheme) -> [Color] {
        (0...20).map { index in
            let value = index * 5
            let color: Color

            switch value {
            case 90...100: color = theme.cpuActivityPeak
            case 80..<90: color = theme.cpuActivityHigh
            case 60..<80: color = theme.cpuActivityModerate
            default: color = theme.cpuActivityLow
            }

            return color.opacity(0.24 + Double(index) * 0.038)
        }
    }
}

struct CPUHistoryGeometry {
    let history: SelectedCPUHistory
    let intervals: [CPUHistoryInterval]

    var selected: CPUHistoryInspection? {
        guard !history.cpus.isEmpty,
              let index = history.observations.firstIndex(where: {
                  $0.observation.identity == history.observation.identity
              }) else { return nil }

        return CPUHistoryInspection(row: 0, observationIndex: index)
    }
    var selectedFraction: Double { fraction(at: history.observation.wallNs) }
    var previousPage: UInt32? {
        history.offset > 0 ? history.offset - SelectedCPUHistory.cpusPerPage : nil
    }
    var nextPage: UInt32? {
        let next = history.offset + SelectedCPUHistory.cpusPerPage

        return next < history.total ? next : nil
    }

    init(history: SelectedCPUHistory) {
        self.history = history
        var intervals = [CPUHistoryInterval]()
        intervals.reserveCapacity(history.observations.count)

        for (index, sample) in history.observations.enumerated() {
            guard sample.intervalAvailable, let previous = sample.previousObservation else {
                continue
            }

            let firstNs = max(history.firstNs, previous.wallNs)
            let endNs = min(history.endNs, sample.observation.wallNs)

            if firstNs < endNs {
                Self.removeOccludedIntervals(&intervals, firstNs: firstNs)
                intervals.append(CPUHistoryInterval(firstNs: firstNs, endNs: endNs,
                    observationIndex: index))
            }
        }

        self.intervals = intervals
    }

    private static func removeOccludedIntervals(_ intervals: inout [CPUHistoryInterval],
                                                firstNs: UInt64) {
        // Later observations own overlapping wall time, including missing row values.
        for _ in 0..<intervals.count {
            guard let previous = intervals.last, previous.endNs > firstNs else { break }

            intervals.removeLast()
            if previous.firstNs < firstNs {
                intervals.append(CPUHistoryInterval(firstNs: previous.firstNs, endNs: firstNs,
                    observationIndex: previous.observationIndex))
                break
            }
        }
    }

    func fraction(at wallNs: UInt64) -> Double {
        let clipped = min(history.endNs, max(history.firstNs, wallNs))

        return Double(clipped - history.firstNs) / Double(history.endNs - history.firstNs)
    }

    func inspect(row: Int, fraction: Double) -> CPUHistoryInspection? {
        guard history.cpus.indices.contains(row), fraction.isFinite,
              (0..<1).contains(fraction) else { return nil }

        for interval in intervals.reversed() {
            if fraction >= self.fraction(at: interval.firstNs),
               fraction < self.fraction(at: interval.endNs) {
                return CPUHistoryInspection(row: row, observationIndex: interval.observationIndex)
            }
        }

        return nil
    }

    func value(_ inspection: CPUHistoryInspection) -> Double? {
        guard history.cpus.indices.contains(inspection.row),
              history.observations.indices.contains(inspection.observationIndex) else {
            return nil
        }

        return history.cpus[inspection.row].busyPercent[inspection.observationIndex]
    }

    func reading(_ inspection: CPUHistoryInspection) -> String {
        guard let value = value(inspection) else { return "Unavailable" }

        return value.formatted(.number.precision(.fractionLength(0))) + "%"
    }

    func target(_ inspection: CPUHistoryInspection) -> RecordedSelectionTarget? {
        guard history.cpus.indices.contains(inspection.row),
              history.observations.indices.contains(inspection.observationIndex) else {
            return nil
        }

        let point = history.observations[inspection.observationIndex].observation

        return RecordedSelectionTarget(second: point.wallNs / 1_000_000_000,
            identity: point.identity)
    }

    func step(from inspection: CPUHistoryInspection?, observations: Int,
              rows: Int) -> CPUHistoryInspection? {
        precondition((-1...1).contains(observations))
        precondition((-1...1).contains(rows))
        guard let current = inspection ?? selected else { return nil }

        return CPUHistoryInspection(row: min(history.cpus.count - 1, max(0, current.row + rows)),
            observationIndex: min(history.observations.count - 1,
                max(0, current.observationIndex + observations)))
    }
}

struct CPUHistoryInterval {
    let firstNs: UInt64
    let endNs: UInt64
    let observationIndex: Int
}

struct CPUHistoryInspection: Equatable {
    let row: Int
    let observationIndex: Int
}
