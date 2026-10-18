import SwiftUI

struct RecordedMemoryPressure: View {
    @Environment(\.flightdeckTheme) private var theme
    let selection: Selection
    @Binding var inspectedID: String?
    @Binding var inspecting: Bool
    let busy: Bool
    let onSelect: (RecordedSelectionTarget) -> Void
    @FocusState private var focused: Bool

    var body: some View {
        Panel(title: "Memory pressure", explanation: "macOS’s Normal, Warning or Critical verdict, shown as one snapshot per observed second in the last 15 minutes. A repeated second shows the selected observation, otherwise the latest reading. Missing or unrecognized readings are blank.") {
            if let timeline = MemoryPressureTimeline(selection: selection) {
                plot(timeline)
                Text(readout(timeline)).font(theme.compactReadingFont).foregroundStyle(theme.muted)
                legend
            } else {
                Text("Memory pressure history is outside the display bounds.")
                    .font(theme.chartStateFont).foregroundStyle(theme.muted)
            }
        }
    }

    private func plot(_ timeline: MemoryPressureTimeline) -> some View {
        let input = interaction(timeline)

        return Canvas { context, size in draw(context: &context, size: size, timeline: timeline) }
            .frame(height: theme.chartHeight)
            .overlay(GeometryReader { proxy in
                Color.clear.contentShape(Rectangle())
                    .onContinuousHover { phase in
                        switch phase {
                        case .active(let point): input.inspect(at: point, size: proxy.size)
                        case .ended:
                            if !focused { input.clear() }
                        }
                    }
                    .onTapGesture { point in
                        guard !busy else { return }

                        focused = true
                        input.click(at: point, size: proxy.size)
                    }
            })
            .focusable(!busy).focused($focused)
            .overlay(RoundedRectangle(cornerRadius: theme.controlRadius)
                .stroke(focused ? theme.focus : .clear, lineWidth: 2))
            .onKeyPress(.leftArrow) { input.step(offset: -1) }
            .onKeyPress(.rightArrow) { input.step(offset: 1) }
            .onKeyPress(.home) { input.boundary(first: true) }
            .onKeyPress(.end) { input.boundary(first: false) }
            .onKeyPress(.escape) { input.clear() }
            .onKeyPress(.return) { input.select() }
            .accessibilityElement(children: .ignore)
            .accessibilityIdentifier("memory-pressure-timeline")
            .accessibilityLabel("Memory pressure, observed states by second")
            .accessibilityValue(readout(timeline))
            .accessibilityHint("Left and Right inspect individual observations, including alternatives in one second. Home and End inspect the bounds. Return selects the exact inspected observation. Escape clears inspection.")
    }

    private var legend: some View {
        HStack(spacing: theme.fieldGap) {
            ForEach(MemoryPressureState.allCases, id: \.rawValue) { state in
                HStack(spacing: theme.fieldGap / 2) {
                    Rectangle().fill(stateColor(state)).frame(width: 9, height: 9)
                        .accessibilityHidden(true)
                    Text(state.label)
                }
            }
            Text("Blank: unavailable")
            Spacer(minLength: 0)
        }.font(theme.compactReadingFont).foregroundStyle(theme.muted)
    }

    private func interaction(_ timeline: MemoryPressureTimeline) -> MemoryPressureInteraction {
        MemoryPressureInteraction(timeline: timeline, inspectedID: $inspectedID,
            inspecting: $inspecting, busy: busy, onSelect: onSelect)
    }

    private func readout(_ timeline: MemoryPressureTimeline) -> String {
        let mode = inspecting ? "Inspecting observation" : "Selected observation"
        guard let point = timeline.point(id: interaction(timeline).readoutID) else {
            return mode + ": Unavailable"
        }

        let state = timeline.state(id: point.id)?.label ?? "Unavailable"
        let date = Date(timeIntervalSince1970: Double(point.wallNs / 1_000_000_000)
            + Double(point.wallNs % 1_000_000_000) / 1e9)
            .formatted(.dateTime.hour().minute().second().secondFraction(.fractional(3)))

        return "\(mode): \(state) · \(date)"
    }

    private func stateColor(_ state: MemoryPressureState) -> Color {
        switch state {
        case .normal: theme.memoryPressureNormal
        case .warning: theme.memoryPressureWarning
        case .critical: theme.memoryPressureCritical
        }
    }

    private func draw(context: inout GraphicsContext, size: CGSize,
                       timeline: MemoryPressureTimeline) {
        let frame = MemoryPressurePlotLayout.frame(size: size)
        guard frame.width > 0 else { return }

        context.fill(Path(frame), with: .color(theme.background))

        let palette = MemoryPressureState.allCases.map { stateColor($0) }
        var paths = Array(repeating: Path(), count: palette.count)

        for (cell, index) in timeline.cells.enumerated() {
            guard let index, let code = timeline.observations[index].pressure,
                  let state = MemoryPressureState(rawValue: code) else { continue }

            let rectangle = CGRect(x: frame.minX + Double(cell) / Double(timeline.cells.count)
                * frame.width, y: frame.minY, width: frame.width / Double(timeline.cells.count),
                height: frame.height)

            paths[Int(state.rawValue)].addRect(rectangle)
        }

        for index in palette.indices {
            context.fill(paths[index], with: .color(palette[index]))
        }

        drawAxis(context: &context, frame: frame, timeline: timeline)
        drawMarker(context: &context, frame: frame,
            fraction: timeline.fraction(id: timeline.selectedID), inspecting: false)
        if inspecting {
            drawMarker(context: &context, frame: frame,
                fraction: timeline.fraction(id: inspectedID), inspecting: true)
        }
    }

    private func drawAxis(context: inout GraphicsContext, frame: CGRect,
                           timeline: MemoryPressureTimeline) {
        for tick in 0...2 {
            let second = timeline.firstSecond
                + (timeline.endSecond - timeline.firstSecond) * UInt64(tick) / 2
            let text = Date(timeIntervalSince1970: Double(second))
                .formatted(.dateTime.hour().minute().second())
            let anchor: UnitPoint = tick == 0 ? .leading : (tick == 2 ? .trailing : .center)

            context.draw(Text(text).font(theme.chartAxisFont).foregroundStyle(theme.muted),
                at: CGPoint(x: frame.minX + Double(tick) / 2 * frame.width,
                    y: frame.maxY + 16), anchor: anchor)
        }
    }

    private func drawMarker(context: inout GraphicsContext, frame: CGRect,
                             fraction: Double?, inspecting: Bool) {
        guard let fraction else { return }

        let x = frame.minX + fraction * frame.width
        var path = Path()
        path.move(to: CGPoint(x: x, y: frame.minY - 4))
        path.addLine(to: CGPoint(x: x, y: frame.maxY + 4))

        context.stroke(path, with: .color(inspecting ? theme.focus : theme.text),
            style: StrokeStyle(lineWidth: 1, dash: inspecting ? [3, 3] : []))
    }
}

@MainActor
struct MemoryPressureInteraction {
    let timeline: MemoryPressureTimeline
    @Binding var inspectedID: String?
    @Binding var inspecting: Bool
    let busy: Bool
    let onSelect: (RecordedSelectionTarget) -> Void

    var readoutID: String? { inspecting ? inspectedID : timeline.selectedID }

    func inspect(at point: CGPoint, size: CGSize) {
        guard !busy else { return }

        let frame = MemoryPressurePlotLayout.frame(size: size)

        inspecting = true
        inspectedID = nil

        if frame.contains(point), frame.width > 0 {
            inspectedID = timeline.point(at: (point.x - frame.minX) / frame.width)?.id
        }
    }

    @discardableResult
    func click(at point: CGPoint, size: CGSize) -> KeyPress.Result {
        guard !busy else { return .ignored }

        inspect(at: point, size: size)

        return select()
    }

    func step(offset: Int) -> KeyPress.Result {
        guard !busy, let point = timeline.step(id: readoutID, offset: offset) else {
            return .ignored
        }

        inspectedID = point.id
        inspecting = true

        return .handled
    }

    func boundary(first: Bool) -> KeyPress.Result {
        guard !busy, let point = timeline.boundary(first: first) else { return .ignored }

        inspectedID = point.id
        inspecting = true

        return .handled
    }

    @discardableResult
    func clear() -> KeyPress.Result {
        guard !busy else { return .ignored }

        resetInspection()

        return .handled
    }

    func select() -> KeyPress.Result {
        guard !busy, let target = timeline.target(id: readoutID) else { return .ignored }

        resetInspection()
        onSelect(target)

        return .handled
    }

    private func resetInspection() {
        inspectedID = nil
        inspecting = false
    }
}

private enum MemoryPressurePlotLayout {
    static func frame(size: CGSize) -> CGRect {
        CGRect(x: 50, y: max(8, (size.height - 52) / 2),
            width: max(0, size.width - 115), height: 28)
    }
}

struct MemoryPressureTimeline {
    static let observationsMax = 4096
    let firstSecond: UInt64
    let endSecond: UInt64
    let observations: [Metric]
    let cells: [Int?]
    let selectedID: String?

    init?(selection: Selection) {
        guard selection.contextSeconds == 900,
              selection.trends.count <= Self.observationsMax,
              selection.second <= UInt64.max / 1_000_000_000 else { return nil }

        firstSecond = selection.second >= 899 ? selection.second - 899 : 0
        endSecond = selection.second + 1
        observations = Self.visibleObservations(selection, firstSecond: firstSecond,
            endSecond: endSecond)
        selectedID = RecordedChartContext(selection: selection).selectedID
        var cells = [Int?](repeating: nil, count: Int(endSecond - firstSecond))
        var identities = Set<String>()

        for (index, point) in observations.enumerated() {
            guard identities.insert(point.id).inserted else { return nil }

            let cell = Int(point.wallNs / 1_000_000_000 - firstSecond)
            if let previous = cells[cell], observations[previous].id == selectedID { continue }

            cells[cell] = index
        }

        self.cells = cells
    }

    private static func visibleObservations(_ selection: Selection, firstSecond: UInt64,
                                             endSecond: UInt64) -> [Metric] {
        selection.trends.filter { point in
            let second = point.wallNs / 1_000_000_000

            return second >= firstSecond && second < endSecond
        }.sorted {
            ($0.wallNs, $0.run, $0.boot, $0.publication)
                < ($1.wallNs, $1.run, $1.boot, $1.publication)
        }
    }

    func point(at fraction: Double) -> Metric? {
        guard fraction.isFinite, (0..<1).contains(fraction) else { return nil }

        let cell = Int(fraction * Double(cells.count))
        guard let index = cells[cell] else { return nil }

        return observations[index]
    }

    func point(id: String?) -> Metric? {
        guard let id else { return nil }

        return observations.first { $0.id == id }
    }

    func state(id: String?) -> MemoryPressureState? {
        guard let code = point(id: id)?.pressure else { return nil }

        return MemoryPressureState(rawValue: code)
    }

    func target(id: String?) -> RecordedSelectionTarget? {
        guard let point = point(id: id) else { return nil }

        return RecordedSelectionTarget(second: point.wallNs / 1_000_000_000,
            identity: point.identity)
    }

    func step(id: String?, offset: Int) -> Metric? {
        precondition((-1...1).contains(offset))
        guard !observations.isEmpty else { return nil }

        let current = observations.firstIndex { $0.id == (id ?? selectedID) }
            ?? observations.count - 1
        let next = min(observations.count - 1, max(0, current + offset))

        return observations[next]
    }

    func boundary(first: Bool) -> Metric? {
        first ? observations.first : observations.last
    }

    func fraction(id: String?) -> Double? {
        guard let point = point(id: id) else { return nil }

        let elapsed = point.wallNs / 1_000_000_000 - firstSecond
        let fraction = Double(point.wallNs % 1_000_000_000) / 1e9

        return (Double(elapsed) + fraction) / Double(cells.count)
    }
}

enum MemoryPressureState: UInt64, CaseIterable {
    case normal = 0, warning = 1, critical = 2

    var label: String {
        switch self {
        case .normal: "Normal"
        case .warning: "Warning"
        case .critical: "Critical"
        }
    }
}
