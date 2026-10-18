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
                    Rectangle().fill(theme.memoryPressureColor(state)).frame(width: 9, height: 9)
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

    private func draw(context: inout GraphicsContext, size: CGSize,
                       timeline: MemoryPressureTimeline) {
        let frame = RecordedStatePlotLayout.frame(size: size)
        guard frame.width > 0 else { return }

        context.fill(Path(frame), with: .color(theme.background))

        let palette = MemoryPressureState.allCases.map { theme.memoryPressureColor($0) }
        var paths = Array(repeating: Path(), count: palette.count)

        for (cell, index) in timeline.cells.enumerated() {
            guard let index, let state = timeline.state(at: index) else { continue }

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

typealias MemoryPressureTimeline = RecordedStateTimeline<MemoryPressureState>
typealias MemoryPressureInteraction = RecordedStateInteraction<MemoryPressureState>

extension RecordedStateTimeline where State == MemoryPressureState {
    init?(selection: Selection) {
        self.init(selection: selection) { metric in
            metric.pressure.flatMap { MemoryPressureState(rawValue: $0) }
        }
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

extension FlightdeckTheme {
    func memoryPressureColor(_ state: MemoryPressureState) -> Color {
        switch state {
        case .normal: memoryPressureNormal
        case .warning: memoryPressureWarning
        case .critical: memoryPressureCritical
        }
    }
}
