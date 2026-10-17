import SwiftUI

struct ReferenceAttributionMetric {
    let key: String
    let title: String
    let unit: String
    let scale: Double
    let color: String
}

struct ReferenceAttribution: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    let panel: ReferencePanel

    static let metrics = [
        ReferenceAttributionMetric(key: "appCpu", title: "CPU", unit: "% of one core",
                                   scale: 1, color: "#5794f2"),
        ReferenceAttributionMetric(key: "appMemory", title: "Resident memory", unit: "GiB",
                                   scale: 1 / 1_073_741_824, color: "#73bf69"),
        ReferenceAttributionMetric(key: "appNetwork", title: "Network", unit: "MB/s",
                                   scale: 1e-6, color: "#00ffff"),
        ReferenceAttributionMetric(key: "appDisk", title: "Disk I/O", unit: "MB/s",
                                   scale: 1e-6, color: "#fade2a"),
        ReferenceAttributionMetric(key: "appGpu", title: "GPU time", unit: "% of interval",
                                   scale: 1, color: "#b877d9"),
        ReferenceAttributionMetric(key: "appEnergy", title: "Process power", unit: "W",
                                   scale: 1, color: "#ff9830")
    ]

    private var metric: ReferenceAttributionMetric? {
        Self.metrics.first { $0.key == panel.source }
    }
    private var rows: [ReferenceRankedApp] {
        referenceRankedApps(model.snapshot?.series[panel.source ?? ""] ?? [],
                            selected: model.window.selected)
    }

    var body: some View {
        let ranked = rows
        return ReferenceSpecialCard(title: panel.title, help: panel.help) {
            Text(panel.unit).font(.caption).foregroundStyle(theme.muted)
            if ranked.isEmpty {
                Text("No active app readings.").font(.callout).foregroundStyle(theme.muted)
                    .frame(maxWidth: .infinity, minHeight: 60, alignment: .leading)
            } else {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(ranked) { row in rank(row, maximum: ranked.first?.value ?? row.value) }
                    }
                }.frame(height: min(280, Double(ranked.count) * 34))
            }
        }
    }

    private func rank(_ row: ReferenceRankedApp, maximum: Double) -> some View {
        let color = theme.referenceColor(metric?.color ?? "#808080")
        return Button { model.inspect(app: row.app) } label: {
            HStack(spacing: 8) {
                Text(row.app).lineLimit(1).truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                GeometryReader { geometry in
                    ZStack(alignment: .leading) {
                        Rectangle().fill(theme.border.opacity(0.45))
                        Rectangle().fill(color.opacity(0.35))
                            .frame(width: geometry.size.width * row.value / maximum)
                        Text(referenceAppReading(row.value, key: metric?.key ?? ""))
                            .font(theme.compactReadingFont).monospacedDigit()
                            .frame(maxWidth: .infinity, alignment: .trailing).padding(.horizontal, 5)
                    }.clipShape(RoundedRectangle(cornerRadius: 4))
                }.frame(height: 24)
            }.font(theme.readingFont).foregroundStyle(theme.text)
                .padding(.vertical, 5).contentShape(Rectangle())
        }.buttonStyle(.plain)
            .overlay(alignment: .bottom) { Rectangle().fill(theme.border).frame(height: 0.5) }
            .accessibilityLabel("Inspect " + row.app + ", "
                                + referenceAppReading(row.value, key: metric?.key ?? ""))
    }
}

struct ReferenceRankedApp: Identifiable {
    let app: String
    let value: Double
    var id: String { app }
}

func referenceRankedApps(_ series: [ReferenceSeries], selected: Int) -> [ReferenceRankedApp] {
    series.compactMap { row in
        guard row.values.indices.contains(selected), let value = row.values[selected],
              value.isFinite, value > 0, let app = row.app else { return nil }

        return ReferenceRankedApp(app: app, value: value)
    }.sorted { $0.value > $1.value }.prefix(10).map { $0 }
}

struct ReferenceCores: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    let panel: ReferencePanel

    private var history: Bool { panel.kind == "heatmap" }
    var body: some View {
        ReferenceSpecialCard(title: panel.title, help: panel.help) {
            Canvas { context, size in
                if history { drawHistory(&context, size: size) }
                else { drawSelected(&context, size: size) }
            }.frame(height: 244)
                .modifier(ReferenceSpecialTimeSelection(enabled: history, left: 104, right: 8))
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(history ? "18 CPU core histories" : "CPU cores at the selected time")
                .accessibilityValue(panel.lines.map { line in
                    line.name + " " + dashboardFixedReading(sample(line, model.window.selected), unit: "%")
                }.joined(separator: "; "))
            Text(history ? "Green → yellow → red: 0–100% utilization. Missing readings remain gaps."
                         : "100% is one core. Selected captured sample.")
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func drawSelected(_ context: inout GraphicsContext, size: CGSize) {
        guard !panel.lines.isEmpty else { return }

        let column = max(0, size.width - 8) / Double(panel.lines.count)
        for (index, line) in panel.lines.enumerated() {
            let value = sample(line, model.window.selected)
            for segment in 0..<20 {
                let level = Double(20 - segment) * 5
                let illuminated = value.map { $0 >= level || (level == 5 && $0 > 0) } ?? false
                let rectangle = CGRect(x: 4 + Double(index) * column,
                                       y: 12 + Double(segment) * 10.5,
                                       width: max(1, column - 2), height: 8.8)
                context.fill(Path(roundedRect: rectangle, cornerRadius: 1.5),
                             with: .color(referenceThresholdColor(level - 1, theme: theme)
                                .opacity(illuminated ? 0.9 : 0.12)))
            }
            context.draw(Text("\(index)").font(theme.chartAxisFont).foregroundStyle(theme.text),
                         at: CGPoint(x: 4 + (Double(index) + 0.5) * column, y: 237))
        }
    }

    private func drawHistory(_ context: inout GraphicsContext, size: CGSize) {
        guard !panel.lines.isEmpty else { return }

        let plot = CGRect(x: 104, y: 10, width: max(1, size.width - 112), height: 204)
        let rowHeight = plot.height / Double(panel.lines.count)
        let columnWidth = plot.width / Double(model.window.upper - model.window.lower + 1)
        for (row, line) in panel.lines.enumerated() {
            let y = plot.minY + Double(row) * rowHeight
            context.draw(Text(coreLabel(line.name)).font(theme.compactReadingFont).foregroundStyle(theme.text),
                         at: CGPoint(x: plot.minX - 9, y: y + rowHeight * 0.5), anchor: .trailing)
            for index in model.window.lower...model.window.upper {
                let rectangle = CGRect(x: plot.minX + Double(index - model.window.lower) * columnWidth,
                                       y: y + 0.5, width: columnWidth + 0.01,
                                       height: max(1, rowHeight - 0.7))
                let color = sample(line, index).map { referenceContinuousColor($0, theme: theme) }
                    ?? theme.border.opacity(0.45)
                context.fill(Path(rectangle), with: .color(color))
            }
        }
        referenceSpecialCursor(&context, plot: plot, selected: model.window.selected,
                               lower: model.window.lower, upper: model.window.upper, color: theme.text)
        drawTimeLabels(&context, plot: plot, height: size.height, model: model, theme: theme)
    }

    private func coreLabel(_ name: String) -> String {
        let parts = name.components(separatedBy: " · ")
        guard let first = parts.first, let cluster = parts.last, parts.count == 2 else { return name }

        return cluster + " " + first.replacingOccurrences(of: "Core ", with: "")
    }
}

struct ReferenceUtilization: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme

    private var resources: [(String, Double?)] {
        [("Performance cores", model.value("cpuUsage1")), ("Super cores", model.value("cpuUsage2")),
         ("GPU", model.value("gpu0")),
         ("Memory", referenceRatio(model.value("memoryUsed0"), capacity: model.value("installed"))),
         ("Swap", referenceRatio(model.value("swapDetail0"), capacity: model.value("swapDetail1")))]
    }

    var body: some View {
        ReferenceSpecialCard(title: "Utilization by resource", help: "pressure") {
            VStack(spacing: theme.fieldGap) {
                ForEach(Array(resources.enumerated()), id: \.offset) { _, row in
                    HStack(spacing: 8) {
                        Text(row.0).font(theme.compactReadingFont).frame(width: 116, alignment: .leading)
                        GeometryReader { geometry in
                            let fraction = min(100, max(0, row.1 ?? 0)) / 100
                            ZStack(alignment: .leading) {
                                Capsule().fill(theme.border)
                                Capsule().fill(utilizationGradient(row.1))
                                    .frame(width: geometry.size.width * fraction)
                            }
                        }.frame(height: 7)
                        Text(dashboardFixedReading(row.1, unit: "%", digits: 0))
                            .font(theme.compactReadingFont).monospacedDigit()
                            .foregroundStyle(row.1.map { referenceThresholdColor($0, theme: theme) }
                                             ?? theme.muted)
                            .frame(width: row.1 == nil ? 72 : 42, alignment: .trailing)
                    }.accessibilityElement(children: .combine)
                }
            }
            HStack {
                verdict("Memory pressure", key: "pressure0", states: ["Normal", "Warning", "Critical"])
                Spacer()
                verdict("Thermal state", key: "thermal0", states: ["Nominal", "Fair", "Serious", "Critical"])
            }.padding(.top, theme.fieldGap)
            Text("Utilization: 0–100%. Display thresholds: 60 / 80 / 90%. Verdicts come from macOS.")
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func verdict(_ title: String, key: String, states: [String]) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(.caption).foregroundStyle(theme.muted)
            Text(referenceState(model.value(key), states: states)).font(.headline)
        }
    }

    private func utilizationGradient(_ value: Double?) -> LinearGradient {
        let value = max(0, value ?? 0)
        if value <= 60 {
            return LinearGradient(colors: [theme.thermalNominal], startPoint: .leading, endPoint: .trailing)
        }
        return LinearGradient(stops: [
            .init(color: theme.thermalNominal, location: 0),
            .init(color: theme.thermalNominal, location: min(1, 60 / value)),
            .init(color: theme.airflowWarm, location: min(1, 80 / value)),
            .init(color: theme.warning, location: min(1, 90 / value)),
            .init(color: theme.airflowHot, location: 1)
        ], startPoint: .leading, endPoint: .trailing)
    }
}

struct ReferenceTimeline: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    let panel: ReferencePanel
    private var states: [String] { panel.states ?? [] }
    private var compact: Bool { panel.id.hasPrefix("overview-") || panel.id == "battery-state" }
    private var selected: Double? { panel.lines.first.flatMap { sample($0, model.window.selected) } }

    var body: some View {
        ReferenceSpecialCard(title: panel.title, help: panel.help) {
            Canvas { context, size in
                let plot = CGRect(x: 70, y: 14, width: max(1, size.width - 78),
                                  height: max(1, size.height - 51))
                let width = plot.width / Double(model.window.upper - model.window.lower + 1)
                context.fill(Path(plot), with: .color(theme.border.opacity(0.3)))
                if let line = panel.lines.first {
                    let runs = referenceStateRuns(line.values,
                        range: model.window.lower...model.window.upper, stateCount: states.count)
                    for run in runs {
                        if let state = run.state, let color = stateColor(state) {
                            let rectangle = CGRect(x: plot.minX + Double(run.lower - model.window.lower) * width,
                                                   y: plot.minY, width: Double(run.upper - run.lower + 1) * width,
                                                   height: plot.height)
                            context.fill(Path(rectangle), with: .color(color.opacity(0.85)))
                            if states.count == 2 && rectangle.width > 90 {
                                context.draw(Text(states[Int(state)]).font(theme.chartStateFont)
                                    .foregroundStyle(theme.background),
                                    at: CGPoint(x: rectangle.midX, y: rectangle.midY))
                            }
                        }
                    }
                }
                context.draw(Text(panel.title == "Memory pressure" ? "Pressure" : states.count == 2 ? "State" : "Thermal")
                    .font(theme.compactReadingFont).foregroundStyle(theme.text),
                    at: CGPoint(x: plot.minX - 8, y: plot.midY), anchor: .trailing)
                referenceSpecialCursor(&context, plot: plot, selected: model.window.selected,
                                       lower: model.window.lower, upper: model.window.upper, color: theme.text)
                drawTimeLabels(&context, plot: plot, height: size.height, model: model, theme: theme)
            }.frame(height: compact ? 96 : 212)
                .modifier(ReferenceSpecialTimeSelection(enabled: true, left: 70, right: 8))
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(panel.title + " history")
                .accessibilityValue(referenceState(selected, states: states))
            Text(referenceSpecialTime(model.date(model.window.selected)) + " · "
                 + referenceState(selected, states: states) + " · " + states.joined(separator: " / "))
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func stateColor(_ value: Double) -> Color? {
        guard value.isFinite, value.rounded() == value, value >= 0,
              value < Double(states.count) else { return nil }

        let colors: [Color]
        if states.count == 2 { colors = [theme.cpuSeries, theme.thermalNominal] }
        else if states.count == 3 { colors = [theme.thermalNominal, theme.airflowWarm, theme.airflowHot] }
        else { colors = [theme.thermalNominal, theme.airflowWarm, theme.warning, theme.airflowHot] }
        return colors.indices.contains(Int(value)) ? colors[Int(value)] : nil
    }
}

struct ReferenceStateRun {
    let lower: Int
    var upper: Int
    let state: Double?
}

func referenceStateRuns(_ values: [Double?], range: ClosedRange<Int>,
                        stateCount: Int) -> [ReferenceStateRun] {
    var runs: [ReferenceStateRun] = []
    for index in range {
        let value = sample(values, index).flatMap {
            $0.rounded() == $0 && $0 >= 0 && $0 < Double(stateCount) ? $0 : nil
        }
        if let last = runs.last, last.state == value {
            runs[runs.count - 1].upper = index
        } else {
            runs.append(ReferenceStateRun(lower: index, upper: index, state: value))
        }
    }
    return runs
}

struct ReferenceQuantities: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    let panel: ReferencePanel

    var body: some View {
        ReferenceSpecialCard(title: panel.title, help: panel.help) {
            ForEach(Array((panel.quantities ?? []).enumerated()), id: \.offset) { _, quantity in
                if quantity.count == 2 {
                    HStack(spacing: theme.fieldGap) {
                        Text(quantity[0]).foregroundStyle(theme.muted)
                        Spacer()
                        Text(referenceQuantityValue(quantity[1], snapshot: model.snapshot,
                                                    selected: model.window.selected))
                            .monospacedDigit().textSelection(.enabled)
                    }.padding(.vertical, 8)
                        .overlay(alignment: .bottom) { Rectangle().fill(theme.border).frame(height: 0.5) }
                        .accessibilityElement(children: .combine)
                }
            }
        }
    }
}

func referenceQuantityValue(_ key: String, snapshot: ReferenceSnapshot?, selected: Int) -> String {
    let direct: [String: (source: String, unit: String, digits: Int)] = [
        "tempSourceSoc": ("temperature0", " °C", 1), "daysUp": ("machineInfo2", " days", 1),
        "tempCpu": ("temperatures0", " °C", 1), "tempGpu": ("temperatures1", " °C", 1),
        "tempSoc": ("temperatures2", " °C", 1), "tempMemory": ("temperatures3", " °C", 1),
        "tempStorage": ("temperatures4", " °C", 1), "tempVoltage": ("temperatures5", " °C", 1),
        "tempDisplay": ("temperatures6", " °C", 1), "tempBoard": ("temperatures7", " °C", 1),
        "tempWireless": ("temperatures8", " °C", 1), "tempAmbient": ("temperatures9", " °C", 1),
        "batteryCharge": ("battery0", "%", 1), "batteryHealth": ("batteryHealth0", "%", 1),
        "batteryCycles": ("batteryHealth1", "", 0), "batteryFlow": ("batteryPower0", " W", 1)
    ]
    if let source = direct[key] {
        return dashboardFixedReading(snapshot?.value(source.source, at: selected),
                                       unit: source.unit, digits: source.digits)
    }
    if key.hasPrefix("mem") {
        let kinds = ["memApp": "app", "memWired": "wired", "memCompressed": "compressed",
                     "memCached": "cached", "memFree": "free"]
        let row = snapshot?.series["memory"]?.first { $0.labels["kind"] == kinds[key] }
        return dashboardFixedReading(row.flatMap { sample($0.values, selected) }.map { $0 / 1_073_741_824 },
                                       unit: " GiB")
    }
    if key == "fanChannel0" || key == "fanChannel1" {
        let channel = key == "fanChannel0" ? "0" : "1"
        let row = snapshot?.series["fans0"]?.first { $0.labels["fan"] == channel }
        return dashboardFixedReading(row.flatMap { sample($0.values, selected) }, unit: " rpm", digits: 0)
    }
    if key == "pressure" {
        return referenceState(snapshot?.value("pressure0", at: selected), states: ["Normal", "Warning", "Critical"])
    }
    if key == "thermal" {
        return referenceState(snapshot?.value("thermal0", at: selected), states: ["Nominal", "Fair", "Serious", "Critical"])
    }
    if key == "adapterRating" {
        return referenceAdaptiveReading(snapshot?.value("batteryPower1", at: selected), unit: "W")
    }
    return "Unavailable"
}

struct ReferenceComputerSources: View {
    @Environment(\.flightdeckTheme) private var theme
    @State private var expanded = true

    var body: some View {
        DisclosureGroup("Schematic reference", isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: theme.fieldGap) {
                Text("MacBook Pro 16″ · 2026 · Apple M5 Pro · Mac17,8 · A3428")
                Text("Captured hardware reference. Bottom view with the cover removed. Hinge at the top.")
                Text("Airflow tint: left CPU-area temperature; right GPU-area temperature.")
                HStack(spacing: theme.fieldGap) {
                    temperatureKey(50, color: theme.airflowCool)
                    temperatureKey(70, color: theme.airflowWarm)
                    temperatureKey(90, color: theme.airflowHot)
                }
                Text("Leaders use component anchors. Physical sensor coordinates and fan-channel sides are unverified. CPU and GPU share the cooling assembly. Separate airflow colors are a display choice, not measured exhaust temperatures. Rotor and particle speed follows each channel’s RPM. Airflow is illustrative, not a measured path or speed.")
                Text("Apple’s 2026 service illustrations establish the installed assembly layout: the board and cooling assembly near the hinge, two blowers, six battery cells and side speaker assemblies. They cover M5 Pro and M5 Max; this drawing simplifies the installed assembly.")
                Text("[Logic board service view](https://support.apple.com/en-us/125827)")
                Text("[Fan procedure](https://support.apple.com/en-us/125832)")
                Text("[Exploded view](https://support.apple.com/en-us/125838)")
                Text("CPU, GPU and memory leaders reference the covered processor assembly. A Counterpoint M5 Pro teardown identifies separate CPU and GPU dies in one package using a 14-inch machine. It does not establish their orientation in this 16-inch chassis. Individual memory and NAND outlines remain unverified.")
                Text("[Counterpoint M5 Pro chip teardown](https://counterpointresearch.com/en/insights/apple-m5-pro-chip-teardown-analysis)")
                Text("Apple’s manual reuses 2024 detached-board and fan photos; iFixit also discloses earlier-model photos. Those photos support assembly context, not exact hidden M5 Pro chip placement. The 2026 installed locator anchors the major layout and cooling contour. No complete electrical schematic or sensor-coordinate map was verified in the prototype research.")
                Text("[iFixit logic board replacement](https://www.ifixit.com/Guide/MacBook+Pro+16-Inch+2026+Logic+Board+Replacement/223848)")
            }.font(.callout).foregroundStyle(theme.muted).padding(.top, theme.fieldGap)
        }.font(.headline).foregroundStyle(theme.text).padding(theme.panelPadding)
            .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
            .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
    }

    private func temperatureKey(_ value: Int, color: Color) -> some View {
        HStack(spacing: 5) {
            Rectangle().fill(color.opacity(0.55)).frame(width: 16, height: 8)
            Text("\(value) °C").font(.caption)
        }
    }
}

private struct ReferenceSpecialCard<Content: View>: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    let help: String?
    @ViewBuilder var content: Content
    @State private var about = false

    var body: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            HStack {
                Text(title).font(.headline)
                Spacer()
                if help != nil {
                    Button { about = true } label: { Image(systemName: "info.circle") }
                        .buttonStyle(.borderless).accessibilityLabel("About " + title)
                }
            }
            content
        }.foregroundStyle(theme.text).padding(theme.panelPadding)
            .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
            .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
            .popover(isPresented: $about) {
                VStack(alignment: .leading, spacing: theme.fieldGap) {
                    ForEach(Array((model.snapshot?.help[help ?? ""] ?? []).enumerated()), id: \.offset) { _, text in
                        Text(text).font(.callout)
                    }
                }.padding(theme.contentPadding).frame(width: 440)
            }
    }
}

private struct ReferenceSpecialTimeSelection: ViewModifier {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    let enabled: Bool
    let left: CGFloat
    let right: CGFloat
    @FocusState private var focused: Bool

    func body(content: Content) -> some View {
        content.overlay(GeometryReader { geometry in
            if enabled {
                Color.clear.contentShape(Rectangle())
                    .onContinuousHover { phase in
                        if case .active(let point) = phase { select(x: point.x, width: geometry.size.width) }
                    }
                    .onTapGesture { point in select(x: point.x, width: geometry.size.width) }
            }
        }).focusable(enabled).focused($focused)
            .overlay(RoundedRectangle(cornerRadius: theme.controlRadius)
                .stroke(focused ? theme.focus : .clear, lineWidth: 2))
            .onKeyPress(.leftArrow) { model.inspect(sample: model.window.selected - 1); return .handled }
            .onKeyPress(.rightArrow) { model.inspect(sample: model.window.selected + 1); return .handled }
            .onKeyPress(.home) { model.inspect(sample: model.window.lower); return .handled }
            .onKeyPress(.end) { model.inspect(sample: model.window.upper); return .handled }
            .onKeyPress(.escape) { model.inspect(sample: model.window.upper); return .handled }
            .accessibilityHint(enabled ? "Arrow keys inspect shared captured time. Home and End select endpoints." : "")
    }

    private func select(x: CGFloat, width: CGFloat) {
        let fraction = min(1, max(0, (x - left) / max(1, width - left - right)))
        model.inspectCell(fraction: fraction)
    }
}

private func referenceRatio(_ value: Double?, capacity: Double?) -> Double? {
    guard let value, value.isFinite, let capacity, capacity.isFinite, capacity > 0 else { return nil }

    return value / capacity * 100
}

private func referenceThresholdColor(_ value: Double, theme: FlightdeckTheme) -> Color {
    if value >= 90 { return theme.airflowHot }
    if value >= 80 { return theme.warning }
    if value >= 60 { return theme.airflowWarm }
    return theme.thermalNominal
}

private func referenceContinuousColor(_ value: Double, theme: FlightdeckTheme) -> Color {
    let fraction = min(100, max(0, value)) / 50
    if fraction < 1 { return theme.thermalNominal.mix(with: theme.airflowWarm, by: fraction) }
    return theme.airflowWarm.mix(with: theme.airflowHot, by: fraction - 1)
}

private func referenceState(_ value: Double?, states: [String]) -> String {
    guard let value, value.isFinite, value.rounded() == value,
          value >= 0, value < Double(states.count) else { return "Unavailable" }

    return states[Int(value)]
}

private func sample(_ line: ReferenceLine, _ index: Int) -> Double? { sample(line.values, index) }
private func sample(_ values: [Double?], _ index: Int) -> Double? {
    guard values.indices.contains(index), let value = values[index], value.isFinite else { return nil }

    return value
}

private func referenceAppReading(_ value: Double, key: String) -> String {
    switch key {
    case "appNetwork", "appDisk":
        let magnitude = abs(value)
        let scale = magnitude >= 1e9 ? 1e9 : magnitude >= 1e6 ? 1e6 : magnitude >= 1e3 ? 1e3 : 1
        let unit = scale == 1e9 ? " GB/s" : scale == 1e6 ? " MB/s" : scale == 1e3 ? " kB/s" : " B/s"
        return dashboardFixedReading(value / scale, unit: unit)
    case "appMemory": return referenceAdaptiveReading(value / 1_073_741_824, unit: "GiB")
    case "appEnergy": return referenceAdaptiveReading(value, unit: "W")
    default: return dashboardFixedReading(value, unit: "%")
    }
}

private func referenceAdaptiveReading(_ value: Double?, unit: String) -> String {
    guard let value, value.isFinite else { return "Unavailable" }

    var scaled = value
    var suffix = unit
    if unit == "GiB" && abs(value) < 1 {
        scaled = value * 1024
        suffix = value == 0 ? "B" : "MiB"
    }
    if unit == "W" && abs(value) > 0 && abs(value) < 1 {
        scaled = value * 1000
        suffix = "mW"
    }
    let absolute = abs(scaled)
    let digits = absolute == 0 || absolute >= 100 ? 0 : absolute >= 1 ? 1
        : min(6, max(2, 1 - Int(floor(log10(absolute)))))
    return scaled.formatted(.number.precision(.fractionLength(0...digits))) + " " + suffix
}

private func referenceSpecialTime(_ date: Date) -> String {
    date.formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits).second(.twoDigits))
}

private func referenceSpecialCursor(_ context: inout GraphicsContext, plot: CGRect,
                                    selected: Int, lower: Int, upper: Int, color: Color) {
    let x = plot.minX + Double(selected - lower) / Double(upper - lower + 1) * plot.width
    var cursor = Path()
    cursor.move(to: CGPoint(x: x, y: plot.minY))
    cursor.addLine(to: CGPoint(x: x, y: plot.maxY))
    context.stroke(cursor, with: .color(color.opacity(0.8)),
                   style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
}

@MainActor
private func drawTimeLabels(_ context: inout GraphicsContext, plot: CGRect, height: CGFloat,
                            model: ReferenceModel, theme: FlightdeckTheme) {
    let indices = [model.window.lower, model.window.upper]
    for (offset, index) in indices.enumerated() {
        context.draw(Text(model.date(index).formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)))
            .font(theme.compactReadingFont).foregroundStyle(theme.muted),
            at: CGPoint(x: offset == 0 ? plot.minX : plot.maxX, y: height - 10),
            anchor: offset == 0 ? .leading : .trailing)
    }
}
