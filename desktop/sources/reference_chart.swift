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
        Canvas { context, size in
            let plot = CGRect(x: 50, y: 24, width: max(1, size.width - 115), height: max(1, size.height - 54))
            let domains = domains()
            for row in 0...4 {
                let y = plot.minY + Double(row) / 4 * plot.height
                var grid = Path(); grid.move(to: CGPoint(x: plot.minX, y: y)); grid.addLine(to: CGPoint(x: plot.maxX, y: y))
                context.stroke(grid, with: .color(theme.border.opacity(0.65)), lineWidth: 0.5)
                let domain = domains["left"] ?? 0...1
                let number = domain.upperBound - Double(row) / 4 * (domain.upperBound - domain.lowerBound)
                context.draw(Text(axis(number)).font(theme.chartAxisFont).foregroundColor(theme.muted), at: CGPoint(x: 42, y: y), anchor: .trailing)
            }
            var positive = Array(repeating: 0.0, count: max(0, upper + 1))
            var negative = positive
            for line in lines {
                let domain = domains[line.axis ?? "left"] ?? 0...1
                var top: [CGPoint] = [], bottom: [CGPoint] = []
                for index in lower...max(lower, upper) {
                    guard line.values.indices.contains(index), let raw = line.values[index] else {
                        drawSegment(context: &context, top: top, bottom: bottom, line: line)
                        top.removeAll(keepingCapacity: true); bottom.removeAll(keepingCapacity: true)
                        continue
                    }

                    let value = line.negative == true ? -raw : raw
                    let stacked = panel.stack == true && line.stack != false && (line.axis == nil || line.axis == "left")
                    let base = stacked ? (value >= 0 ? positive[index] : negative[index]) : 0
                    let total = base + value
                    if stacked { if value >= 0 { positive[index] = total } else { negative[index] = total } }
                    let x = xPosition(index, plot: plot)
                    top.append(CGPoint(x: x, y: yPosition(total, domain: domain, plot: plot)))
                    bottom.append(CGPoint(x: x, y: yPosition(base, domain: domain, plot: plot)))
                }
                drawSegment(context: &context, top: top, bottom: bottom, line: line)
            }
            if selectedElapsed != nil || (selected >= lower && selected <= upper) {
                let x = selectedElapsed.map { plot.minX + min(1, max(0, $0 / 300)) * plot.width }
                    ?? xPosition(selected, plot: plot)
                var cursor = Path(); cursor.move(to: CGPoint(x: x, y: plot.minY)); cursor.addLine(to: CGPoint(x: x, y: plot.maxY))
                context.stroke(cursor, with: .color(theme.text.opacity(0.4)), style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
            }
            for offset in 0...2 {
                let index = lower + (upper - lower) * offset / 2
                let label = panel.times == nil ? timeLabel(index) : "\(offset * 150 / 60):" + String(format: "%02d", offset * 150 % 60)
                context.draw(Text(label).font(theme.chartAxisFont).foregroundColor(theme.muted),
                    at: CGPoint(x: plot.minX + Double(offset) / 2 * plot.width, y: plot.maxY + 18))
            }
            context.draw(Text(panel.unit).font(theme.chartAxisFont).foregroundColor(theme.muted), at: CGPoint(x: 0, y: 0), anchor: .topLeading)
            for (offset, key) in ["right1", "right2"].enumerated() {
                if let domain = domains[key], let line = lines.first(where: { $0.axis == key }) {
                    context.draw(Text("\(axis(domain.upperBound)) \(line.unit ?? panel.unit)").font(theme.chartSecondaryAxisFont).foregroundColor(theme.referenceColor(line.color)),
                        at: CGPoint(x: plot.maxX + 5, y: plot.minY + Double(offset) * 15), anchor: .topLeading)
                }
            }
        }
    }

    private func domains() -> [String: ClosedRange<Double>] {
        var ranges: [String: (Double, Double)] = [:]
        for line in lines {
            let key = line.axis ?? "left"
            let values = (lower...max(lower, upper)).compactMap { line.values.indices.contains($0) ? line.values[$0] : nil }
                .map { line.negative == true ? -$0 : $0 }
            ranges[key] = (min(ranges[key]?.0 ?? 0, values.min() ?? 0), max(ranges[key]?.1 ?? 0, values.max() ?? 0))
        }
        if panel.stack == true {
            for index in lower...max(lower, upper) {
                let values = lines.filter { $0.stack != false && $0.axis == nil }.compactMap { line -> Double? in
                    guard line.values.indices.contains(index), let value = line.values[index] else { return nil }

                    return line.negative == true ? -value : value
                }
                ranges["left"] = (min(ranges["left"]?.0 ?? 0, values.filter { $0 < 0 }.reduce(0, +)),
                    max(ranges["left"]?.1 ?? 0, values.filter { $0 > 0 }.reduce(0, +)))
            }
        }
        return ranges.mapValues { low, high in min(0, low)...max(1, high * 1.08) }
            .merging(["left": (panel.minimum ?? min(0, ranges["left"]?.0 ?? 0))...max((panel.minimum ?? 0) + 1, panel.maximum ?? max(1, (ranges["left"]?.1 ?? 0) * 1.08))]) { _, new in new }
    }

    private func drawSegment(context: inout GraphicsContext, top: [CGPoint], bottom: [CGPoint], line: ReferenceLine) {
        guard let first = top.first else { return }

        let color = theme.referenceColor(line.color)
        let path = smoothPath(top)
        let opacity = line.fill ?? panel.fill ?? 0.1
        if opacity > 0, !bottom.isEmpty {
            var area = path
            appendSmooth(Array(bottom.reversed()), to: &area, moving: false)
            area.closeSubpath()
            if panel.gradient != false, let highest = top.map(\.y).min(), let lowest = bottom.map(\.y).max() {
                context.fill(area, with: .linearGradient(Gradient(colors: [color.opacity(opacity), color.opacity(opacity * 0.08)]), startPoint: CGPoint(x: 0, y: highest), endPoint: CGPoint(x: 0, y: max(highest + 1, lowest))))
            } else {
                context.fill(area, with: .color(color.opacity(opacity)))
            }
        }
        context.stroke(path, with: .color(color), style: StrokeStyle(lineWidth: panel.lineWidth ?? theme.chartStroke, dash: line.dashed == true ? [5, 4] : []))
        if top.count == 1 { context.fill(Path(ellipseIn: CGRect(x: first.x - 2, y: first.y - 2, width: 4, height: 4)), with: .color(color)) }
    }

    private func smoothPath(_ points: [CGPoint]) -> Path {
        var path = Path()
        appendSmooth(points, to: &path, moving: true)
        return path
    }

    private func appendSmooth(_ points: [CGPoint], to path: inout Path, moving: Bool) {
        guard let first = points.first else { return }

        if moving { path.move(to: first) } else { path.addLine(to: first) }
        for index in 1..<points.count {
            let before = points[index - 1], after = points[index]
            let width = after.x - before.x
            guard width != 0 else { path.addLine(to: after); continue }

            let slope = (after.y - before.y) / width
            let oldSlope = index > 1 ? (before.y - points[index - 2].y) / (before.x - points[index - 2].x) : slope
            let nextSlope = index + 1 < points.count ? (points[index + 1].y - after.y) / (points[index + 1].x - after.x) : slope
            let firstSlope = slope * oldSlope > 0 ? 2 * slope * oldSlope / (slope + oldSlope) : 0
            let lastSlope = slope * nextSlope > 0 ? 2 * slope * nextSlope / (slope + nextSlope) : 0
            path.addCurve(to: after, control1: CGPoint(x: before.x + width / 3, y: before.y + firstSlope * width / 3),
                control2: CGPoint(x: after.x - width / 3, y: after.y - lastSlope * width / 3))
        }
    }

    private func xPosition(_ index: Int, plot: CGRect) -> Double {
        if let times = panel.times, times.indices.contains(index) { return plot.minX + times[index] / 300 * plot.width }
        return plot.minX + Double(index - lower) / Double(max(1, upper - lower)) * plot.width
    }

    private func yPosition(_ value: Double, domain: ClosedRange<Double>, plot: CGRect) -> Double {
        plot.maxY - (value - domain.lowerBound) / (domain.upperBound - domain.lowerBound) * plot.height
    }

    private func timeLabel(_ index: Int) -> String {
        if let times = panel.times, times.indices.contains(index) {
            return "\(Int(times[index]) / 60):" + String(format: "%02d", Int(times[index]) % 60)
        }
        return Date(timeIntervalSince1970: start + Double(index) * step)
            .formatted(.dateTime.hour().minute())
    }

    private func axis(_ value: Double) -> String {
        value.formatted(.number.notation(abs(value) >= 10000 ? .compactName : .automatic).precision(.fractionLength(0...1)))
    }
}
