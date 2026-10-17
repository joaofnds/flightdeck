import SwiftUI

extension FlightdeckTheme {
    func referenceColor(_ value: String) -> Color {
        switch value.lowercased() {
        case "#5794f2": cpuSeries
        case "#73bf69": chartPalette[0]
        case "#b877d9": gpuPowerSeries
        case "#ff9830": systemPowerSeries
        case "#f2495c": temperatureSeries
        case "#f2cc0c", "#fade2a": airflowWarm
        case "#ccccdc", "#ccc": text
        case "#8e8e9e", "#808080", "#8e8e8e": neutralSeries
        case "#00ffff": networkReceivedSeries
        case "#37872d": chartPalette[6]
        case "#e0b400": chartPalette[7]
        case "#1f60c4": chartPalette[8]
        case "#c15c17": chartPalette[9]
        case "#8ab8ff": lightBlueSeries
        case "#ca95e5": lightPurpleSeries
        default: muted
        }
    }
}

struct ReferenceStatistics: Equatable {
    let selected: Double?
    let mean: Double?
    let maximum: Double?
    let count: Int

    init(values: [Double?], selected: Int, range: ClosedRange<Int>) {
        let valid = range.compactMap { values.indices.contains($0) ? values[$0] : nil }
        self.selected = values.indices.contains(selected) ? values[selected] : nil
        mean = valid.isEmpty ? nil : valid.reduce(0, +) / Double(valid.count)
        maximum = valid.max()
        count = valid.count
    }
}

func referenceSelectedReading(line: ReferenceLine, panel: ReferencePanel, index: Int) -> String {
    if panel.times != nil, index < 0 { return "No report" }
    let value = line.values.indices.contains(index) ? line.values[index] : nil
    return dashboardReading(value, unit: line.unit ?? panel.unit, digits: line.precision ?? panel.precision)
}

struct ReferenceChart: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    let panel: ReferencePanel
    @State private var hidden: Set<String> = []
    @State private var inspector = false
    @State private var about = false
    @FocusState private var focused: Bool

    private var range: ClosedRange<Int> { model.window.lower...model.window.upper }
    private var selected: Int {
        guard let times = panel.times else { return model.window.selected }

        let second = Double(model.selfIndex + 1) * 10
        return times.firstIndex(of: second) ?? -1
    }
    private var visible: [ReferenceLine] { panel.lines.filter { !hidden.contains($0.name) } }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            HStack {
                Text(panel.title).font(.headline)
                Spacer()
                Button("Data") { inspector = true }.buttonStyle(.borderless)
                    .accessibilityLabel("Inspect \(panel.title) data")
                Button { about = true } label: { Image(systemName: "info.circle") }
                    .buttonStyle(.borderless).accessibilityLabel("About \(panel.title)")
            }
            ReferencePlot(panel: panel, lines: visible, selected: selected,
                selectedElapsed: panel.times == nil ? nil : Double(model.selfIndex + 1) * 10,
                lower: panel.times == nil ? range.lowerBound : 0,
                upper: panel.times == nil ? range.upperBound : max(0, (panel.lines.first?.values.count ?? 1) - 1),
                start: Double(model.snapshot?.start ?? 0), step: Double(model.snapshot?.step ?? 30))
                .frame(height: theme.chartHeight)
                .overlay(GeometryReader { geometry in
                    Color.clear.contentShape(Rectangle())
                        .onContinuousHover { phase in
                            if case .active(let point) = phase { inspect(x: point.x, width: geometry.size.width) }
                        }
                        .onTapGesture { point in inspect(x: point.x, width: geometry.size.width) }
                })
                .focusable().focused($focused)
                .overlay(RoundedRectangle(cornerRadius: theme.controlRadius)
                    .stroke(focused ? theme.focus : .clear, lineWidth: 2))
                .onKeyPress(.leftArrow) { step(-1); return .handled }
                .onKeyPress(.rightArrow) { step(1); return .handled }
                .onKeyPress(.home) { select(panel.times == nil ? range.lowerBound : 0); return .handled }
                .onKeyPress(.end) { select(panel.times == nil ? range.upperBound : 29); return .handled }
                .onKeyPress(.escape) { select(panel.times == nil ? range.upperBound : 29); return .handled }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(panel.title + ", " + panel.unit)
                .accessibilityValue(visible.map { line in line.name + " " + reading(line) }.joined(separator: "; "))
                .accessibilityHint("Left and Right inspect samples across all charts. Home and End select range bounds.")
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 145), alignment: .leading)], alignment: .leading, spacing: 7) {
                ForEach(Array(panel.lines.enumerated()), id: \.offset) { _, line in
                    Button {
                        if hidden.contains(line.name) { hidden.remove(line.name) }
                        else { hidden.insert(line.name) }
                    } label: {
                        HStack(spacing: 5) {
                            RoundedRectangle(cornerRadius: 1).fill(theme.referenceColor(line.color)).frame(width: 12, height: 3)
                            Text(line.name).lineLimit(1)
                            Text(reading(line)).monospacedDigit().foregroundStyle(theme.muted)
                        }.font(.caption).opacity(hidden.contains(line.name) ? 0.4 : 1)
                    }.buttonStyle(.plain)
                        .accessibilityLabel(line.name + ", " + reading(line))
                        .accessibilityValue(hidden.contains(line.name) ? "Hidden" : "Visible")
                }
            }
            if let caption = panel.caption {
                Text(caption).font(.caption).foregroundStyle(theme.muted)
            }
        }.padding(theme.panelPadding)
            .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
            .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
            .sheet(isPresented: $inspector) { dataSheet }
            .popover(isPresented: $about) {
                VStack(alignment: .leading, spacing: theme.fieldGap) {
                    Text(panel.title).font(.headline)
                    ForEach(Array((model.snapshot?.help[panel.help ?? panel.id] ?? ["Captured source readings; unavailable values remain distinct from measured zero."]).enumerated()), id: \.offset) { _, text in
                        Text(text).font(.callout)
                    }
                }.padding(theme.contentPadding).frame(width: 440)
            }
    }

    private func reading(_ line: ReferenceLine) -> String {
        referenceSelectedReading(line: line, panel: panel, index: selected)
    }

    private func inspect(x: CGFloat, width: CGFloat) {
        let fraction = min(1, max(0, (x - 50) / max(1, width - 115)))
        if let times = panel.times {
            guard !times.isEmpty else { return }

            let time = fraction * 300
            let nearest = times.enumerated().min { abs($0.element - time) < abs($1.element - time) }?.offset ?? 0
            if panel.lines.first?.values.count == 30 { model.inspectDiagnostic(report: nearest) }
            else { model.inspectDiagnostic(report: Int((time / 10).rounded()) - 1) }
        } else {
            model.inspect(sample: range.lowerBound + Int((fraction * Double(range.count - 1)).rounded()))
        }
    }

    private func select(_ index: Int) {
        if panel.times == nil { model.inspect(sample: index) }
        else { model.inspectDiagnostic(report: index) }
    }

    private func step(_ amount: Int) {
        select((panel.times == nil ? model.window.selected : model.selfIndex) + amount)
    }

    private var dataSheet: some View {
        VStack(alignment: .leading, spacing: theme.panelGap) {
            HStack { Text(panel.title).font(theme.titleFont); Spacer(); Button("Close") { inspector = false } }
            Text(panel.times == nil ? "Captured sysmon dashboard · 30-second samples · rates use 1-minute windows" : "Saved Flightdeck diagnostic run · elapsed report times · not live")
                .font(.callout).foregroundStyle(theme.muted)
            Text("Mean and maximum summarize available samples; they are not time-weighted means or between-sample peaks.")
                .font(.caption).foregroundStyle(theme.muted)
            if panel.times == nil {
                Text("Selected \(model.date(model.window.selected).formatted(.dateTime.year().month().day().hour().minute().second().timeZone()))")
                Text("Range \(model.date(range.lowerBound).formatted(.dateTime.hour().minute().second()))–\(model.date(range.upperBound).formatted(.dateTime.hour().minute().second()))")
                    .font(.caption).foregroundStyle(theme.muted)
            } else {
                Text("Selected elapsed report \((model.selfIndex + 1) * 10) seconds · range 0:00–5:00")
                    .font(.caption).foregroundStyle(theme.muted)
            }
            Grid(alignment: .leading, horizontalSpacing: 24, verticalSpacing: 12) {
                GridRow { Text("Series"); Text("Selected"); Text("Mean"); Text("Maximum"); Text("Samples") }.font(.headline)
                ForEach(Array(visible.enumerated()), id: \.offset) { _, line in
                    let bounds = panel.times == nil ? range : 0...max(0, line.values.count - 1)
                    let stats = ReferenceStatistics(values: line.values, selected: selected, range: bounds)
                    GridRow {
                        Text(line.name)
                        Text(reading(line))
                        Text(dashboardReading(stats.mean, unit: line.unit ?? panel.unit, digits: line.precision ?? panel.precision))
                        Text(dashboardReading(stats.maximum, unit: line.unit ?? panel.unit, digits: line.precision ?? panel.precision))
                        Text("\(stats.count) / \(bounds.count)")
                    }.font(theme.tableFont)
                }
            }.textSelection(.enabled)
            ForEach(Array((model.snapshot?.help[panel.help ?? panel.id] ?? []).enumerated()), id: \.offset) { _, text in
                Text(text).font(.callout).foregroundStyle(theme.muted)
            }
        }.padding(theme.contentPadding).frame(minWidth: 680)
            .background(theme.background).foregroundStyle(theme.text)
    }
}

private struct ReferencePlot: View {
    @Environment(\.flightdeckTheme) private var theme
    let panel: ReferencePanel
    let lines: [ReferenceLine]
    let selected: Int
    let selectedElapsed: Double?
    let lower: Int
    let upper: Int
    let start: Double
    let step: Double

    var body: some View {
        let samples = (lower...max(lower, upper)).map { index in
            TimeSeriesSample(id: "reference:\(index)",
                time: panel.times?[index] ?? start + Double(index) * step, segment: "reference")
        }
        let domain = panel.times == nil
            ? (start + Double(lower) * step)...(start + Double(max(lower + 1, upper)) * step)
            : 0...300
        let values = lines.map { line in
            TimeSeriesLine(name: line.name, color: theme.referenceColor(line.color),
                values: samples.indices.map { offset in
                    let index = lower + offset
                    return line.values.indices.contains(index) ? line.values[index] : nil
                }, unit: line.unit, stack: line.stack, negative: line.negative,
                strokePattern: line.dashed == true ? .dashed : .solid,
                axis: line.axis, fill: line.fill)
        }
        if let model = TimeSeriesModel(samples: samples, lines: values, domain: domain) {
            TimeSeriesPlot(model: model, style: TimeSeriesStyle(unit: panel.unit,
                stack: panel.stack, minimum: panel.minimum, maximum: panel.maximum,
                gradient: panel.gradient, lineWidth: panel.lineWidth, fill: panel.fill),
                selectedTime: selectedElapsed.map { min(300, max(0, $0)) }
                    ?? model.selectedTime(id: "reference:\(selected)"),
                labels: (0...2).map { offset in
                    if panel.times != nil {
                        return "\(offset * 150 / 60):" + String(format: "%02d", offset * 150 % 60)
                    }

                    let index = lower + (upper - lower) * offset / 2
                    return Date(timeIntervalSince1970: start + Double(index) * step)
                        .formatted(.dateTime.hour().minute())
                })
        } else {
            Text("Chart readings exceed the display bounds.")
        }
    }
}
