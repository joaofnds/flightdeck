import SwiftUI

struct RecordedBatteryReading: Decodable, Sendable {
    let sourceContinuousNs: UInt64
    let ageSeconds: Double
    let chargePercent: Double?
    let healthPercent: Double?
    let cycleCount: UInt64?
    let charging: Bool?
    let externalPower: Bool?
    let powerWatts: Double?
    let adapterWatts: UInt64?
}

struct RecordedBatteryData {
    let context: RecordedChartContext
    let theme: FlightdeckTheme

    var selected: RecordedBatteryReading? {
        guard let observation = context.selection.selected,
              let summary = context.selection.summary,
              summary.identity == observation.identity,
              summary.wallNs == observation.wallNs else { return nil }

        return summary.battery
    }

    var charge: TimeSeriesLine {
        TimeSeriesLine(name: "Charge", color: theme.batteryChargeSeries,
            values: context.selection.trends.map { $0.battery?.chargePercent }, unit: "%")
    }

    var flow: TimeSeriesLine {
        TimeSeriesLine(name: "Into battery", color: theme.batteryFlowSeries,
            values: context.selection.trends.map { $0.battery?.powerWatts }, unit: "W")
    }

    var details: [(String, String)] {
        [("Health", dashboardReading(selected?.healthPercent, unit: "%")),
         ("Cycles", selected?.cycleCount.map(String.init) ?? "Unavailable"),
         ("Adapter rating", selected?.adapterWatts.map { "\($0) W" } ?? "Unavailable"),
         ("External power", Self.boolean(selected?.externalPower)),
         ("Source age", dashboardReading(selected?.ageSeconds, unit: "s"))]
    }

    static func boolean(_ value: Bool?) -> String {
        value.map { $0 ? "Yes" : "No" } ?? "Unavailable"
    }
}

struct RecordedBattery: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @State private var inspectedID: String?
    @State private var inspecting = false
    @State private var historyExpanded = true
    @State private var stateExpanded = true
    @State private var detailsExpanded = true

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            if let selection = model.selection {
                let data = RecordedBatteryData(context: RecordedChartContext(selection: selection),
                    theme: theme)
                if selection.selected == nil {
                    Label("No recorded tick at this second", systemImage: "clock.badge.questionmark")
                        .foregroundStyle(theme.muted)
                }
                summaries(data)
                DisclosureGroup("Battery history", isExpanded: $historyExpanded) {
                    history(data).padding(.top, theme.fieldGap)
                }.font(.headline)
                DisclosureGroup("Charging history", isExpanded: $stateExpanded) {
                    RecordedBatteryCharging(selection: selection, inspectedID: $inspectedID,
                        inspecting: $inspecting, busy: model.busy, onSelect: model.select)
                        .padding(.top, theme.fieldGap)
                }.font(.headline)
                details(data)
            } else {
                Text(model.busy ? "Reading selected tick…" : "No observation selected.")
                    .foregroundStyle(theme.muted)
            }
        }
        .onChange(of: model.selection?.selected?.identity) { _, _ in resetInspection() }
        .onChange(of: model.selection?.second) { _, _ in resetInspection() }
    }

    private func resetInspection() { inspectedID = nil; inspecting = false }

    private func summaries(_ data: RecordedBatteryData) -> some View {
        MetricSummaryGrid {
            MetricSummary(title: "Charge", reading: dashboardReading(data.selected?.chargePercent,
                unit: "%"), note: "Reported full capacity", color: theme.batteryChargeSeries)
            MetricSummary(title: "Charging", reading: RecordedBatteryData.boolean(data.selected?.charging),
                note: "Battery-controller state", color: theme.batteryCharging)
            MetricSummary(title: "Battery flow", reading: dashboardReading(data.selected?.powerWatts,
                unit: "W"), note: "Positive in · negative out", color: theme.batteryFlowSeries)
            MetricSummary(title: "Health", reading: dashboardReading(data.selected?.healthPercent,
                unit: "%"), note: data.selected?.cycleCount.map { "\($0) cycles" } ?? "Cycles unavailable",
                color: theme.batteryHealth)
        }
    }

    private func history(_ data: RecordedBatteryData) -> some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .top, spacing: theme.panelGap) {
                Self.chargeChart(data, inspectedID: $inspectedID, inspecting: $inspecting)
                    .frame(minWidth: 330)
                Self.flowChart(data, inspectedID: $inspectedID, inspecting: $inspecting)
                    .frame(minWidth: 330)
            }
            VStack(spacing: theme.panelGap) {
                Self.chargeChart(data, inspectedID: $inspectedID, inspecting: $inspecting)
                Self.flowChart(data, inspectedID: $inspectedID, inspecting: $inspecting)
            }
        }
    }

    static func chargeChart(_ data: RecordedBatteryData, inspectedID: Binding<String?>,
                            inspecting: Binding<Bool>) -> RecordedResourceChart {
        RecordedResourceChart(title: "Battery charge", context: data.context,
            lines: [data.charge], style: TimeSeriesStyle(unit: "%", minimum: 0, maximum: 100,
                fill: 0.1), inspectedID: inspectedID, inspecting: inspecting,
            explanation: "Charge as a share of reported full capacity. Recorded once a minute and held for at most 60 seconds in the same run and boot. Missing readings remain gaps.")
    }

    static func flowChart(_ data: RecordedBatteryData, inspectedID: Binding<String?>,
                          inspecting: Binding<Bool>) -> RecordedResourceChart {
        RecordedResourceChart(title: "Battery power flow", context: data.context,
            lines: [data.flow], style: TimeSeriesStyle(unit: "W", fill: 0.1),
            inspectedID: inspectedID, inspecting: inspecting,
            explanation: "Positive is power into the battery; negative is discharge. Derived from controller current and voltage. Adapter rating is a separate capacity, not this measured flow.")
    }

    private func details(_ data: RecordedBatteryData) -> some View {
        DisclosureGroup("Battery details", isExpanded: $detailsExpanded) {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), alignment: .leading)],
                      alignment: .leading, spacing: theme.fieldGap) {
                ForEach(data.details, id: \.0) { name, value in
                    HStack(spacing: theme.fieldGap) {
                        Text(name).foregroundStyle(theme.muted)
                        Spacer(minLength: 0)
                        Text(value).monospacedDigit().foregroundStyle(theme.text)
                    }.font(theme.readingFont)
                }
            }.padding(.top, theme.fieldGap)
            Text("Health is full charge capacity divided by design capacity and can exceed 100%.")
                .font(.callout).foregroundStyle(theme.muted).padding(.top, theme.fieldGap)
        }
        .font(.headline).padding(theme.panelPadding)
        .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
        .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
    }
}

struct RecordedBatteryCharging: View {
    @Environment(\.flightdeckTheme) private var theme
    let selection: Selection
    @Binding var inspectedID: String?
    @Binding var inspecting: Bool
    let busy: Bool
    let onSelect: (RecordedSelectionTarget) -> Void
    @FocusState private var focused: Bool

    var body: some View {
        Panel(title: "Charging state", explanation: "The battery controller’s binary state, shown for each observed second in the last 15 minutes. Repeated seconds retain the exact selected observation; unavailable battery readings and unobserved seconds are blank.") {
            if let timeline = BatteryChargingTimeline(selection: selection) {
                plot(timeline)
                Text(readout(timeline)).font(theme.compactReadingFont).foregroundStyle(theme.muted)
                legend
            } else {
                Text("Charging history is outside the display bounds.")
                    .font(theme.chartStateFont).foregroundStyle(theme.muted)
            }
        }
    }

    private func plot(_ timeline: BatteryChargingTimeline) -> some View {
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
            .accessibilityIdentifier("battery-charging-timeline")
            .accessibilityLabel("Charging state, observed states by second")
            .accessibilityValue(readout(timeline))
            .accessibilityHint("Left and Right inspect individual observations, including alternatives in one second. Home and End inspect the bounds. Return selects the exact inspected observation. Escape clears inspection.")
    }

    private var legend: some View {
        HStack(spacing: theme.fieldGap) {
            ForEach(BatteryChargingState.allCases, id: \.rawValue) { state in
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

    private func interaction(_ timeline: BatteryChargingTimeline) -> BatteryChargingInteraction {
        BatteryChargingInteraction(timeline: timeline, inspectedID: $inspectedID,
            inspecting: $inspecting, busy: busy, onSelect: onSelect)
    }

    private func readout(_ timeline: BatteryChargingTimeline) -> String {
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

    private func stateColor(_ state: BatteryChargingState) -> Color {
        switch state {
        case .notCharging: theme.batteryNotCharging
        case .charging: theme.batteryCharging
        }
    }

    private func draw(context: inout GraphicsContext, size: CGSize,
                       timeline: BatteryChargingTimeline) {
        let frame = RecordedStatePlotLayout.frame(size: size)
        guard frame.width > 0 else { return }

        context.fill(Path(frame), with: .color(theme.background))

        let palette = BatteryChargingState.allCases.map { stateColor($0) }
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
                           timeline: BatteryChargingTimeline) {
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

typealias BatteryChargingTimeline = RecordedStateTimeline<BatteryChargingState>
typealias BatteryChargingInteraction = RecordedStateInteraction<BatteryChargingState>

extension RecordedStateTimeline where State == BatteryChargingState {
    init?(selection: Selection) {
        self.init(selection: selection) { metric in
            metric.battery?.charging.map { BatteryChargingState(charging: $0) }
        }
    }
}

enum BatteryChargingState: UInt64, CaseIterable {
    case notCharging = 0, charging = 1

    init(charging: Bool) { self = charging ? .charging : .notCharging }

    var label: String {
        switch self {
        case .notCharging: "Not charging"
        case .charging: "Charging"
        }
    }
}
