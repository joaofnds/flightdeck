import SwiftUI

struct TimeSeriesSample: Equatable {
    let id: String
    let time: Double
    let segment: String
}

struct TimeSeriesLine {
    let name: String
    let color: Color
    let values: [Double?]
    var unit: String? = nil
    var stack: Bool? = nil
    var negative: Bool? = nil
    var strokePattern: TimeSeriesStrokePattern = .solid
    var axis: String? = nil
    var fill: Double? = nil
}

enum TimeSeriesStrokePattern: Equatable, Sendable {
    case solid
    case dashed
    case dotted
    case custom([CGFloat])

    func style(width: CGFloat) -> StrokeStyle {
        switch self {
        case .solid: return StrokeStyle(lineWidth: width)
        case .dashed: return StrokeStyle(lineWidth: width, dash: [5, 4])
        case .dotted: return StrokeStyle(lineWidth: width, lineCap: .round, dash: [1, 4])
        case let .custom(dash):
            precondition(dash.count <= 8)
            precondition(dash.isEmpty == false)
            precondition(dash.allSatisfy { $0.isFinite })
            precondition(dash.allSatisfy { $0 > 0 })

            return StrokeStyle(lineWidth: width, dash: dash)
        }
    }
}

struct TimeSeriesModel {
    static let observationsMax = 4096
    let samples: [TimeSeriesSample]
    let lines: [TimeSeriesLine]
    let domain: ClosedRange<Double>

    init?(samples: [TimeSeriesSample], lines: [TimeSeriesLine], domain: ClosedRange<Double>) {
        guard samples.count <= Self.observationsMax, lines.count <= 1024,
              samples.count * lines.count <= 262_144,
              domain.lowerBound.isFinite, domain.upperBound.isFinite,
              domain.upperBound > domain.lowerBound,
              (domain.upperBound - domain.lowerBound).isFinite,
              samples.allSatisfy({ $0.time.isFinite && domain.contains($0.time) }),
              Set(samples.map(\.id)).count == samples.count,
              lines.allSatisfy({ line in
                  line.values.count == samples.count
                      && (line.fill.map { $0.isFinite && (0...1).contains($0) } ?? true)
                      && line.values.allSatisfy { value in
                          value.map { $0.isFinite && abs($0) <= Double.greatestFiniteMagnitude / 2048 }
                              ?? true
                      }
              }) else { return nil }

        self.samples = samples
        self.lines = lines
        self.domain = domain
    }

    func selectedTime(id: String) -> Double? {
        samples.first { $0.id == id }?.time
    }

    func segments(line: Int) -> [[Int]] {
        guard lines.indices.contains(line) else { return [] }

        var result: [[Int]] = []
        var current: [Int] = []
        for index in samples.indices {
            let previous = index > 0 ? samples[index - 1] : nil
            let connected = previous.map {
                $0.segment == samples[index].segment && $0.time < samples[index].time
            } ?? false
            if !connected || lines[line].values[index] == nil {
                if !current.isEmpty { result.append(current); current = [] }
            }
            if lines[line].values[index] != nil { current.append(index) }
        }
        if !current.isEmpty { result.append(current) }
        return result
    }
}

struct TimeSeriesStyle {
    let unit: String
    var stack: Bool? = nil
    var minimum: Double? = nil
    var maximum: Double? = nil
    var gradient: Bool? = nil
    var lineWidth: Double? = nil
    var fill: Double? = nil
    var interpolation: TimeSeriesInterpolation = .monotone

    var isValid: Bool {
        let bounds = [minimum, maximum].compactMap { $0 }
        guard bounds.allSatisfy({ $0.isFinite && abs($0) <= Double.greatestFiniteMagnitude / 2048 }),
              fill.map({ $0.isFinite && (0...1).contains($0) }) ?? true,
              lineWidth.map({ $0.isFinite && $0 > 0 && $0 <= 100 }) ?? true else { return false }

        if let minimum, let maximum { return maximum > minimum }

        return true
    }
}

struct TimeSeriesPlot: View {
    @Environment(\.flightdeckTheme) private var theme
    let model: TimeSeriesModel
    let style: TimeSeriesStyle
    let selectedTime: Double?
    let labels: [String]

    private var lines: [TimeSeriesLine] { model.lines }

    var body: some View {
        if style.isValid {
            Canvas { context, size in
                let plot = Self.frame(size: size)
                let domains = domains()
                for row in 0...4 {
                    let y = plot.minY + Double(row) / 4 * plot.height
                    var grid = Path(); grid.move(to: CGPoint(x: plot.minX, y: y)); grid.addLine(to: CGPoint(x: plot.maxX, y: y))
                    context.stroke(grid, with: .color(theme.border.opacity(0.65)), lineWidth: 0.5)
                    let domain = domains["left"] ?? 0...1
                    let number = domain.upperBound - Double(row) / 4 * (domain.upperBound - domain.lowerBound)
                    context.draw(Text(axis(number)).font(theme.chartAxisFont).foregroundColor(theme.muted), at: CGPoint(x: 42, y: y), anchor: .trailing)
                }
                var positive = Array(repeating: 0.0, count: model.samples.count)
                var negative = positive
                for (lineIndex, line) in lines.enumerated() {
                    let domain = domains[line.axis ?? "left"] ?? 0...1
                    for segment in model.segments(line: lineIndex) {
                        var top: [CGPoint] = [], bottom: [CGPoint] = []
                        for index in segment {
                            guard let raw = line.values[index] else { continue }

                            let value = line.negative == true ? -raw : raw
                            let stacked = style.stack == true && line.stack != false && (line.axis == nil || line.axis == "left")
                            let base = stacked ? (value >= 0 ? positive[index] : negative[index]) : 0
                            let total = base + value
                            if stacked { if value >= 0 { positive[index] = total } else { negative[index] = total } }
                            let x = xPosition(index, plot: plot)
                            top.append(CGPoint(x: x, y: yPosition(total, domain: domain, plot: plot)))
                            bottom.append(CGPoint(x: x, y: yPosition(base, domain: domain, plot: plot)))
                        }
                        drawSegment(context: &context, top: top, bottom: bottom, line: line)
                    }
                }
                if let selectedTime, selectedTime.isFinite, model.domain.contains(selectedTime) {
                    let x = timePosition(selectedTime, plot: plot)
                    var cursor = Path(); cursor.move(to: CGPoint(x: x, y: plot.minY)); cursor.addLine(to: CGPoint(x: x, y: plot.maxY))
                    context.stroke(cursor, with: .color(theme.text.opacity(0.4)), style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
                }
                for offset in 0...2 {
                    if labels.indices.contains(offset) {
                        context.draw(Text(labels[offset]).font(theme.chartAxisFont).foregroundColor(theme.muted),
                            at: CGPoint(x: plot.minX + Double(offset) / 2 * plot.width, y: plot.maxY + 18))
                    }
                }
                context.draw(Text(style.unit).font(theme.chartAxisFont).foregroundColor(theme.muted), at: CGPoint(x: 0, y: 0), anchor: .topLeading)
                for (offset, key) in ["right1", "right2"].enumerated() {
                    if let domain = domains[key], let line = lines.first(where: { $0.axis == key }) {
                        context.draw(Text("\(axis(domain.upperBound)) \(line.unit ?? style.unit)").font(theme.chartSecondaryAxisFont).foregroundColor(line.color),
                            at: CGPoint(x: plot.maxX + 5, y: plot.minY + Double(offset) * 15), anchor: .topLeading)
                    }
                }
            }
        } else {
            Text("Chart scale is unavailable.")
        }
    }

    nonisolated static func frame(size: CGSize) -> CGRect {
        CGRect(x: 50, y: 24, width: max(1, size.width - 115), height: max(1, size.height - 54))
    }

    private func domains() -> [String: ClosedRange<Double>] {
        var ranges: [String: (Double, Double)] = [:]
        for line in lines {
            let key = line.axis ?? "left"
            let values = model.samples.indices.compactMap { line.values.indices.contains($0) ? line.values[$0] : nil }
                .map { line.negative == true ? -$0 : $0 }
            ranges[key] = (min(ranges[key]?.0 ?? 0, values.min() ?? 0), max(ranges[key]?.1 ?? 0, values.max() ?? 0))
        }
        if style.stack == true {
            for index in model.samples.indices {
                let values = lines.filter { $0.stack != false && $0.axis == nil }.compactMap { line -> Double? in
                    guard line.values.indices.contains(index), let value = line.values[index] else { return nil }

                    return line.negative == true ? -value : value
                }
                ranges["left"] = (min(ranges["left"]?.0 ?? 0, values.filter { $0 < 0 }.reduce(0, +)),
                    max(ranges["left"]?.1 ?? 0, values.filter { $0 > 0 }.reduce(0, +)))
            }
        }
        let lower = style.minimum ?? min(0, ranges["left"]?.0 ?? 0)
        let upper = max(max(lower + 1, lower.nextUp),
            style.maximum ?? max(1, (ranges["left"]?.1 ?? 0) * 1.08))
        return ranges.mapValues { low, high in min(0, low)...max(1, high * 1.08) }
            .merging(["left": lower...upper]) { _, new in new }
    }

    private func drawSegment(context: inout GraphicsContext, top: [CGPoint], bottom: [CGPoint], line: TimeSeriesLine) {
        guard let first = top.first else { return }

        let color = line.color
        var path = Path()
        style.interpolation.append(top, to: &path, moving: true)
        let opacity = line.fill ?? style.fill ?? 0.1
        if opacity > 0, !bottom.isEmpty {
            var area = path
            style.interpolation.append(Array(bottom.reversed()), to: &area, moving: false)
            area.closeSubpath()
            if style.gradient != false, let highest = top.map(\.y).min(), let lowest = bottom.map(\.y).max() {
                context.fill(area, with: .linearGradient(Gradient(colors: [color.opacity(opacity), color.opacity(opacity * 0.08)]), startPoint: CGPoint(x: 0, y: highest), endPoint: CGPoint(x: 0, y: max(highest + 1, lowest))))
            } else {
                context.fill(area, with: .color(color.opacity(opacity)))
            }
        }
        context.stroke(path, with: .color(color),
            style: line.strokePattern.style(width: style.lineWidth ?? theme.chartStroke))
        if top.count == 1 { context.fill(Path(ellipseIn: CGRect(x: first.x - 2, y: first.y - 2, width: 4, height: 4)), with: .color(color)) }
    }

    private func xPosition(_ index: Int, plot: CGRect) -> Double {
        timePosition(model.samples[index].time, plot: plot)
    }

    private func timePosition(_ time: Double, plot: CGRect) -> Double {
        plot.minX + (time - model.domain.lowerBound)
            / (model.domain.upperBound - model.domain.lowerBound) * plot.width
    }

    private func yPosition(_ value: Double, domain: ClosedRange<Double>, plot: CGRect) -> Double {
        plot.maxY - (value - domain.lowerBound) / (domain.upperBound - domain.lowerBound) * plot.height
    }

    private func axis(_ value: Double) -> String {
        value.formatted(.number.notation(abs(value) >= 10000 ? .compactName : .automatic).precision(.fractionLength(0...1)))
    }
}

enum TimeSeriesInterpolation {
    case linear
    case monotone

    func append(_ points: [CGPoint], to path: inout Path, moving: Bool) {
        guard let first = points.first else { return }

        if moving { path.move(to: first) } else { path.addLine(to: first) }
        for index in 1..<points.count {
            let before = points[index - 1], after = points[index]
            if self == .linear { path.addLine(to: after); continue }
            let width = after.x - before.x
            guard width != 0 else { path.addLine(to: after); continue }

            let slope = (after.y - before.y) / width
            let oldSlope = index > 1 ? (before.y - points[index - 2].y) / (before.x - points[index - 2].x) : slope
            let nextSlope = index + 1 < points.count ? (points[index + 1].y - after.y) / (points[index + 1].x - after.x) : slope
            let firstSlope = slope * oldSlope > 0 ? 2 * slope * oldSlope / (slope + oldSlope) : 0
            let lastSlope = slope * nextSlope > 0 ? 2 * slope * nextSlope / (slope + nextSlope) : 0
            let firstControl = CGPoint(x: before.x + width / 3, y: before.y + firstSlope * width / 3)
            let lastControl = CGPoint(x: after.x - width / 3, y: after.y - lastSlope * width / 3)
            if firstControl.y.isFinite, lastControl.y.isFinite {
                path.addCurve(to: after, control1: firstControl, control2: lastControl)
            } else {
                path.addLine(to: after)
            }
        }
    }
}

struct TimeSeriesCoordinates {
    let domain: ClosedRange<Double>
    let size: CGSize

    var frame: CGRect { TimeSeriesPlot.frame(size: size) }

    func time(at location: CGPoint) -> Double? {
        guard location.x.isFinite, location.y.isFinite,
              frame.contains(location) else { return nil }

        return domain.lowerBound + (location.x - frame.minX) / frame.width
            * (domain.upperBound - domain.lowerBound)
    }

    func position(at time: Double) -> Double? {
        guard time.isFinite, domain.contains(time) else { return nil }

        return frame.minX + (time - domain.lowerBound)
            / (domain.upperBound - domain.lowerBound) * frame.width
    }
}
