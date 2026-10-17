import AppKit
import SwiftUI

struct SchematicReadings {
    var cpuTemperature: Double?
    var gpuTemperature: Double?
    var fan0: Double?
    var fan1: Double?
    var cpuPercent: Double?
    var gpuPercent: Double?
    var memoryUsedGiB: Double?
    var memoryTotalGiB: Double?
    var memoryTemperature: Double?
    var batteryPercent: Double?
    var batteryWatts: Double?
    var thermalState: String?
}

struct MacBookSchematic: View {
    @Environment(\.flightdeckTheme) private var theme
    let readings: SchematicReadings
    @State private var controlsOpen = true
    @ObservedObject private var preview: SchematicPreviewState

    init(readings: SchematicReadings, preview: SchematicPreviewState = SchematicPreviewState()) {
        self.readings = readings
        self.preview = preview
    }

    private var overrides: [SchematicOverride: Double] {
        get { preview.overrides }
        nonmutating set { preview.overrides = newValue }
    }

    private var displayed: SchematicReadings {
        var result = readings
        result.cpuTemperature = overrides[.cpuTemperature] ?? readings.cpuTemperature
        result.gpuTemperature = overrides[.gpuTemperature] ?? readings.gpuTemperature
        result.fan0 = overrides[.fan0] ?? readings.fan0
        result.fan1 = overrides[.fan1] ?? readings.fan1
        return result
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.panelGap) {
            HStack {
                Circle().fill(stateColor).frame(width: 7, height: 7)
                Text(overrides.isEmpty ? "Thermal state" : "Readings")
                    .foregroundStyle(theme.muted)
                Text(overrides.isEmpty ? readings.thermalState ?? "Unavailable" : "Simulated")
                Spacer()
                Button("QA controls") { controlsOpen.toggle() }
                    .accessibilityValue(controlsOpen ? "Expanded" : "Collapsed")
            }
            .font(theme.chartStateFont)
            SchematicDiagram(readings: displayed, overridden: Set(overrides.keys))
            if controlsOpen { controls }
        }
        .foregroundStyle(theme.text)
        .padding(theme.panelPadding)
        .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
        .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("MacBook schematic")
    }

    private var stateColor: Color {
        if !overrides.isEmpty { return theme.cpuSeries }
        switch readings.thermalState?.lowercased() {
        case "nominal": return theme.thermalNominal
        case "fair": return theme.airflowWarm
        case "serious": return theme.warning
        case "critical": return theme.airflowHot
        default: return theme.muted
        }
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            HStack(alignment: .top) {
                Text("Drag to override a reading. Charts keep captured data. "
                     + "Slider ranges are for testing, not device limits.")
                    .font(theme.compactReadingFont).foregroundStyle(theme.muted)
                Spacer()
                Button("Reset to recording") { overrides.removeAll() }
                    .disabled(overrides.isEmpty)
            }
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 250))], spacing: theme.fieldGap) {
                ForEach(SchematicOverride.allCases) { field in
                    control(field)
                }
            }
        }
        .padding(.top, theme.fieldGap)
        .accessibilityLabel("Temporary QA controls")
    }

    private func control(_ field: SchematicOverride) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text(field.label).foregroundStyle(theme.muted)
                Spacer()
                Text(dashboardFixedReading(field.value(in: displayed), unit: field.unit,
                                      digits: field.digits))
                    .monospacedDigit()
            }
            .font(theme.compactReadingFont)
            Slider(value: Binding(
                get: { field.sliderValue(in: displayed) },
                set: { overrides[field] = $0 }
            ), in: field.range, step: field.step)
            .accessibilityLabel(field.label + " QA override")
            .accessibilityValue(dashboardFixedReading(field.value(in: displayed), unit: field.unit,
                                                 digits: field.digits))
        }
    }
}

@MainActor
final class SchematicPreviewState: ObservableObject {
    @Published var overrides: [SchematicOverride: Double] = [:]
}

enum SchematicOverride: String, CaseIterable, Identifiable {
    case cpuTemperature, gpuTemperature, fan0, fan1
    var id: String { rawValue }
    var isTemperature: Bool { self == .cpuTemperature || self == .gpuTemperature }
    var range: ClosedRange<Double> { isTemperature ? 20...110 : 0...8000 }
    var step: Double { isTemperature ? 0.1 : 1 }
    var digits: Int { isTemperature ? 1 : 0 }
    var unit: String { isTemperature ? " °C" : " rpm" }
    var label: String {
        switch self {
        case .cpuTemperature: return "CPU temperature"
        case .gpuTemperature: return "GPU temperature"
        case .fan0: return "Fan 0 speed"
        case .fan1: return "Fan 1 speed"
        }
    }

    func value(in readings: SchematicReadings) -> Double? {
        switch self {
        case .cpuTemperature: return readings.cpuTemperature
        case .gpuTemperature: return readings.gpuTemperature
        case .fan0: return readings.fan0
        case .fan1: return readings.fan1
        }
    }

    func sliderValue(in readings: SchematicReadings) -> Double {
        guard let value = value(in: readings), value.isFinite else { return range.lowerBound }

        return min(range.upperBound, max(range.lowerBound, value))
    }
}

private struct SchematicDiagram: View {
    @Environment(\.flightdeckTheme) private var theme
    let readings: SchematicReadings
    let overridden: Set<SchematicOverride>

    var body: some View {
        ViewThatFits(in: .horizontal) {
            SchematicDiagramFrame(rails: true) { diagram(rails: true) }.frame(minWidth: 850)
            SchematicDiagramFrame(rails: false) { diagram(rails: false) }
        }
    }

    private func diagram(rails: Bool) -> some View {
        GeometryReader { geometry in
            let layout = SchematicLayout(size: geometry.size, rails: rails)
            ZStack(alignment: .topLeading) {
                SchematicAssembly(readings: readings)
                    .frame(width: layout.stage.width, height: layout.stage.height)
                    .offset(x: layout.stage.minX, y: layout.stage.minY)
                Canvas { context, _ in
                    for callout in SchematicCallout.allCases {
                        let leader = layout.leader(for: callout)
                        var line = Path()
                        line.move(to: leader.anchor)
                        line.addLine(to: leader.elbow)
                        line.addLine(to: leader.port)
                        context.stroke(line, with: .color(callout.color(theme).opacity(0.65)),
                                       lineWidth: 1)
                        let pin = CGRect(x: leader.anchor.x - 2, y: leader.anchor.y - 2,
                                         width: 4, height: 4)
                        context.fill(Path(ellipseIn: pin), with: .color(callout.color(theme)))
                    }
                }
                .allowsHitTesting(false).accessibilityHidden(true)
                ForEach(SchematicCallout.allCases) { callout in
                    label(callout)
                        .frame(width: layout.labelWidth,
                               alignment: callout.isLeft ? .leading : .trailing)
                        .offset(x: layout.labelOrigin(for: callout).x,
                                y: layout.labelOrigin(for: callout).y)
                }
            }
        }
    }

    private func label(_ callout: SchematicCallout) -> some View {
        let values = callout.values(readings)
        let simulated = callout.override.map { overridden.contains($0) } ?? false
        return VStack(alignment: callout.isLeft ? .leading : .trailing, spacing: 0) {
            Text(callout.title + (simulated ? " · QA" : ""))
                .font(theme.schematicLabelFont)
                .tracking(0.9).foregroundStyle(theme.muted)
                .frame(height: 18)
            Text(values.primary).font(theme.schematicValueFont)
                .monospacedDigit().lineLimit(1).minimumScaleFactor(0.65)
                .frame(height: 30)
            Text(values.secondary).font(theme.compactReadingFont).monospacedDigit()
                .foregroundStyle(theme.muted).frame(height: 18)
        }
        .accessibilityElement(children: .combine)
        .accessibilityHint(simulated ? "QA override" : callout.scope)
    }
}

private enum SchematicCallout: String, CaseIterable, Identifiable {
    case fan0, fan1, cpu, gpu, memory, battery
    var id: String { rawValue }
    var isLeft: Bool { self == .fan0 || self == .cpu || self == .memory }
    var title: String {
        switch self {
        case .fan0: return "FAN 0"
        case .fan1: return "FAN 1"
        case .cpu: return "CPU"
        case .gpu: return "GPU"
        case .memory: return "MEMORY"
        case .battery: return "BATTERY"
        }
    }
    var anchor: CGPoint {
        switch self {
        case .fan0: return CGPoint(x: 183, y: 221)
        case .fan1: return CGPoint(x: 657, y: 221)
        case .cpu: return CGPoint(x: 400, y: 238)
        case .gpu: return CGPoint(x: 440, y: 238)
        case .memory: return CGPoint(x: 380, y: 228)
        case .battery: return CGPoint(x: 608, y: 458)
        }
    }
    var rise: CGFloat {
        switch self {
        case .fan0, .fan1: return -55
        case .cpu, .gpu: return -195
        case .memory: return 132
        case .battery: return 60
        }
    }
    var override: SchematicOverride? {
        switch self {
        case .fan0: return .fan0
        case .fan1: return .fan1
        case .cpu: return .cpuTemperature
        case .gpu: return .gpuTemperature
        case .memory, .battery: return nil
        }
    }
    var scope: String {
        switch self {
        case .fan0, .fan1: return "Fan channel side is unverified"
        case .cpu: return "Hottest CPU-area sensor; covered processor assembly anchor"
        case .gpu: return "Hottest GPU-area sensor; covered processor assembly anchor"
        case .memory: return "Memory area maximum; covered processor assembly reference"
        case .battery: return "Complete battery pack; component anchor"
        }
    }

    func color(_ theme: FlightdeckTheme) -> Color {
        switch self {
        case .fan0, .fan1, .cpu: return theme.cpuSeries
        case .gpu: return theme.gpuPowerSeries
        case .memory: return theme.schematicMemory
        case .battery: return theme.systemPowerSeries
        }
    }

    func values(_ readings: SchematicReadings) -> (primary: String, secondary: String) {
        switch self {
        case .fan0: return fanValues(readings.fan0)
        case .fan1: return fanValues(readings.fan1)
        case .cpu:
            return (dashboardFixedReading(readings.cpuTemperature, unit: " °C"),
                    "Utilization " + dashboardFixedReading(readings.cpuPercent, unit: "%"))
        case .gpu:
            return (dashboardFixedReading(readings.gpuTemperature, unit: " °C"),
                    "Utilization " + dashboardFixedReading(readings.gpuPercent, unit: "%"))
        case .memory:
            let used = dashboardFixedReading(readings.memoryUsedGiB, unit: "")
            let total = dashboardFixedReading(readings.memoryTotalGiB, unit: " GiB", digits: 0)
            let available = readings.memoryUsedGiB?.isFinite == true
                && readings.memoryTotalGiB?.isFinite == true
                && (readings.memoryTotalGiB ?? 0) > 0
            return (available ? used + " / " + total : "Unavailable",
                    "Peak " + dashboardFixedReading(readings.memoryTemperature, unit: " °C"))
        case .battery:
            let watts = readings.batteryWatts
            let milliwatts = watts.map { $0 != 0 && abs($0) < 0.1 } ?? false
            return (dashboardFixedReading(readings.batteryPercent, unit: "%"),
                    "Power " + dashboardFixedReading(milliwatts ? watts.map { $0 * 1000 } : watts,
                                                unit: milliwatts ? " mW" : " W"))
        }
    }

    private func fanValues(_ rpm: Double?) -> (primary: String, secondary: String) {
        guard let rpm, rpm.isFinite, rpm >= 0 else { return ("Unavailable", "Unavailable") }

        return (dashboardFixedReading(rpm, unit: " rpm", digits: 0), rpm == 0 ? "Stopped" : "Running")
    }
}

private struct SchematicDiagramFrame: Layout {
    let rails: Bool

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews,
                      cache: inout ()) -> CGSize {
        precondition(subviews.count == 1)
        let width = max(0, proposal.width ?? (rails ? 850 : 320))
        let height = rails ? width * 686 / 1188 : width * 600 / 840 + 320
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize,
                       subviews: Subviews, cache: inout ()) {
        precondition(subviews.count == 1)
        subviews[0].place(at: bounds.origin, proposal: ProposedViewSize(bounds.size))
    }
}

private struct SchematicLayout {
    let size: CGSize
    let rails: Bool
    var labelWidth: CGFloat { rails ? 158 : max(0, size.width / 2 - 24) }
    var stage: CGRect {
        if rails {
            let scale = size.width / 1188
            return CGRect(x: 174 * scale, y: 50 * scale,
                          width: 840 * scale, height: 600 * scale)
        }
        return CGRect(x: 0, y: 216, width: size.width, height: 600 * size.width / 840)
    }

    func leader(for callout: SchematicCallout)
        -> (anchor: CGPoint, elbow: CGPoint, port: CGPoint) {
        let scale = stage.width / 840
        let anchor = CGPoint(x: stage.minX + callout.anchor.x * scale,
                             y: stage.minY + callout.anchor.y * scale)
        let direction: CGFloat = callout.isLeft ? -1 : 1
        if rails {
            let rise = callout.rise * scale
            let elbow = CGPoint(x: anchor.x + direction * abs(rise), y: anchor.y + rise)
            return (anchor, elbow, CGPoint(x: callout.isLeft ? 0 : size.width, y: elbow.y))
        }
        let origin = labelOrigin(for: callout)
        let below = callout == .memory || callout == .battery
        let fraction: CGFloat
        switch callout {
        case .fan0: fraction = 0
        case .fan1: fraction = 1
        case .cpu: fraction = 0.46
        case .gpu: fraction = 0.54
        case .memory: fraction = 0.25
        case .battery: fraction = 0.88
        }
        let port = CGPoint(x: size.width * fraction,
                           y: origin.y + (below ? -8 : 74))
        let elbow = CGPoint(x: port.x, y: anchor.y + (below ? 1 : -1) * abs(port.x - anchor.x))
        return (anchor, elbow, port)
    }

    func labelOrigin(for callout: SchematicCallout) -> CGPoint {
        let left: CGFloat = callout.isLeft ? 0 : size.width - labelWidth
        if rails {
            let scale = stage.width / 840
            let top = stage.minY + (callout.anchor.y + callout.rise) * scale - 73
            return CGPoint(x: left, y: max(0, top))
        }
        switch callout {
        case .fan0, .fan1: return CGPoint(x: left, y: 98)
        case .cpu, .gpu: return CGPoint(x: left, y: 0)
        case .memory, .battery: return CGPoint(x: left, y: stage.maxY + 36)
        }
    }
}

private struct SchematicAssembly: View {
    @Environment(\.flightdeckTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reducedMotion
    @Environment(\.scenePhase) private var scenePhase
    let readings: SchematicReadings
    @State private var visible = false
    @State private var motion0 = SchematicFanMotion()
    @State private var motion1 = SchematicFanMotion()

    init(readings: SchematicReadings) {
        self.readings = readings
        _motion0 = State(initialValue: SchematicFanMotion(rpm: readings.fan0))
        _motion1 = State(initialValue: SchematicFanMotion(rpm: readings.fan1))
    }

    private var animates: Bool { visible && !reducedMotion && scenePhase == .active }
    private var running: Bool { motion0.rpm > 0 || motion1.rpm > 0 }

    var body: some View {
        ZStack {
            if let image = SchematicArtwork.chassis {
                Image(nsImage: image).resizable().interpolation(.high)
                    .aspectRatio(840.0 / 600.0, contentMode: .fit)
                TimelineView(.animation(minimumInterval: 1.0 / 30,
                                        paused: !animates || !running)) { timeline in
                    Canvas { context, size in
                        context.scaleBy(x: size.width / 840, y: size.height / 600)
                        drawFan(&context, right: false, motion: motion0,
                                temperature: readings.cpuTemperature, date: timeline.date)
                        drawFan(&context, right: true, motion: motion1,
                                temperature: readings.gpuTemperature, date: timeline.date)
                    }
                }
                .allowsHitTesting(false)
            } else {
                Text("Schematic artwork unavailable").foregroundStyle(theme.muted)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("MacBook Pro 16-inch internal assembly, bottom cover removed, "
                            + "hinge at the top. Airflow is illustrative.")
        .onAppear { visible = true; updateMotion() }
        .onDisappear { visible = false; updateMotion() }
        .onScrollVisibilityChange(threshold: 0.01) { visible = $0; updateMotion() }
        .onChange(of: readings.fan0) { updateMotion() }
        .onChange(of: readings.fan1) { updateMotion() }
        .onChange(of: animates) { updateMotion() }
    }

    private func updateMotion() {
        let now = Date()
        motion0.set(rpm: readings.fan0, enabled: animates, at: now)
        motion1.set(rpm: readings.fan1, enabled: animates, at: now)
    }

    private func drawFan(_ context: inout GraphicsContext, right: Bool,
                         motion: SchematicFanMotion, temperature: Double?, date: Date) {
        var fan = context
        if right { fan.translateBy(x: 840, y: 0); fan.scaleBy(x: -1, y: 1) }
        if let rotor = SchematicArtwork.rotor {
            var spinning = fan
            spinning.translateBy(x: 183, y: 221)
            spinning.rotate(by: .degrees(-360 * motion.turns(at: date)))
            spinning.draw(Image(nsImage: rotor), in: CGRect(x: -52, y: -52, width: 104, height: 104))
        }
        if motion.rpm > 0 {
            drawAirflow(&fan, motion: motion, color: airflowColor(temperature), date: date)
        }
    }

    private func drawAirflow(_ context: inout GraphicsContext, motion: SchematicFanMotion,
                             color: Color, date: Date) {
        context.clip(to: SchematicAirflow.boundary)
        context.clip(to: Path(ellipseIn: CGRect(x: 154, y: 192, width: 58, height: 58)),
                     options: .inverse)
        let gradient = Gradient(stops: [
            .init(color: color.opacity(0), location: 0),
            .init(color: color.opacity(0.11), location: 0.08),
            .init(color: color.opacity(0.11), location: 0.97),
            .init(color: color.opacity(0), location: 1)
        ])
        context.fill(SchematicAirflow.boundary,
                     with: .linearGradient(gradient, startPoint: CGPoint(x: 0, y: 33),
                                           endPoint: CGPoint(x: 0, y: 319)))
        if !reducedMotion {
            drawParticles(&context, motion: motion, color: color, date: date)
        }
    }

    private func drawParticles(_ context: inout GraphicsContext, motion: SchematicFanMotion,
                               color: Color, date: Date) {
        for lane in 0..<24 {
            let route = SchematicAirflow.routes[lane]
            for phase in 0..<4 {
                let variation = (Double(lane) * 0.618034 + Double(phase) * 0.381966)
                    .truncatingRemainder(dividingBy: 1)
                let offset = (Double(phase) + (Double(lane) * 0.618034)
                    .truncatingRemainder(dividingBy: 1)) / 4
                let progress = (motion.distance(at: date) / route.length + offset)
                    .truncatingRemainder(dividingBy: 1)
                let point = route.position(at: progress)
                let opacity = min(1, progress / 0.06, (1 - progress) / 0.08)
                let radius = 0.65 + variation * 0.45
                let particle = CGRect(x: point.x - radius, y: point.y - radius,
                                      width: radius * 2, height: radius * 2)
                context.fill(Path(ellipseIn: particle),
                             with: .color(color.opacity(opacity * (0.36 + variation * 0.24))))
            }
        }
    }

    private func airflowColor(_ temperature: Double?) -> Color {
        guard let temperature, temperature.isFinite else { return theme.muted }

        if temperature <= 50 { return theme.airflowCool }
        if temperature >= 90 { return theme.airflowHot }
        if temperature < 70 {
            return theme.airflowCool.mix(with: theme.airflowWarm, by: (temperature - 50) / 20)
        }
        return theme.airflowWarm.mix(with: theme.airflowHot, by: (temperature - 70) / 20)
    }
}

struct SchematicFanMotion {
    private(set) var rpm = 0.0
    private var enabled = false
    private var date = Date(timeIntervalSinceReferenceDate: 0)
    private var completedTurns = 0.0
    private var completedDistance = 0.0

    init(rpm: Double? = nil) {
        self.rpm = rpm.flatMap { $0.isFinite && $0 >= 0 ? $0 : nil } ?? 0
    }

    mutating func set(rpm: Double?, enabled: Bool, at date: Date) {
        completedTurns = turns(at: date)
        completedDistance = distance(at: date)
        self.date = date
        self.rpm = rpm.flatMap { $0.isFinite && $0 >= 0 ? $0 : nil } ?? 0
        self.enabled = enabled
    }

    func turns(at date: Date) -> Double {
        completedTurns + elapsed(at: date) * rpm / (rpm + 1500) / 4
    }

    func distance(at date: Date) -> Double {
        completedDistance + elapsed(at: date) * 160 * min(rpm / 1500, 8)
    }

    private func elapsed(at date: Date) -> Double {
        enabled ? max(0, date.timeIntervalSince(self.date)) : 0
    }
}

@MainActor
private enum SchematicArtwork {
    static let chassis = load("schematic_chassis")
    static let rotor = load("schematic_rotor")

    private static func load(_ name: String) -> NSImage? {
        guard let directory = Bundle.main.resourceURL else { return nil }

        return NSImage(contentsOf: directory.appendingPathComponent(name + ".png"))
    }
}

private enum SchematicAirflow {
    static let boundary: Path = {
        var path = housing
        path.addRect(CGRect(x: 112, y: 33, width: 177, height: 51))
        return path
    }()
    static let housing: Path = {
        var path = Path()
        path.move(to: CGPoint(x: 109, y: 83))
        path.addLine(to: CGPoint(x: 292, y: 83))
        path.addLine(to: CGPoint(x: 292, y: 221))
        path.addCurve(to: CGPoint(x: 194, y: 319), control1: CGPoint(x: 292, y: 277),
                      control2: CGPoint(x: 251, y: 319))
        path.addCurve(to: CGPoint(x: 102, y: 227), control1: CGPoint(x: 141, y: 319),
                      control2: CGPoint(x: 102, y: 281))
        path.addLine(to: CGPoint(x: 102, y: 174))
        path.addCurve(to: CGPoint(x: 123, y: 140), control1: CGPoint(x: 102, y: 157),
                      control2: CGPoint(x: 110, y: 150))
        path.addLine(to: CGPoint(x: 109, y: 140))
        path.closeSubpath()
        return path
    }()
    static let perimeter = makePerimeter()
    static let routes = (0..<24).map { SchematicAirflowRoute(lane: $0, perimeter: perimeter) }

    private static func makePerimeter() -> [CGPoint] {
        var points: [CGPoint] = []
        housing.forEach { element in
            switch element {
            case .move(to: let point):
                precondition(points.isEmpty)
                points.append(point)
            case .line(to: let point):
                points.append(point)
            case .curve(to: let end, control1: let first, control2: let second):
                appendCurve(to: &points, end: end, first: first, second: second)
            case .quadCurve:
                preconditionFailure("The blower housing must use cubic curves.")
            case .closeSubpath:
                break
            }
        }
        precondition(!points.isEmpty)

        return points
    }

    private static func appendCurve(to points: inout [CGPoint], end: CGPoint,
                                    first: CGPoint, second: CGPoint) {
        let start = points[points.count - 1]
        for step in 1...32 {
            points.append(schematicBezier(start: start, first: first, second: second,
                                           end: end, progress: Double(step) / 32))
        }
    }
}

struct SchematicAirflowRoute {
    let points: [CGPoint]
    let distances: [Double]
    var length: Double { distances[distances.count - 1] }

    init(lane: Int) {
        self.init(lane: lane, perimeter: SchematicAirflow.perimeter)
    }

    fileprivate init(lane: Int, perimeter: [CGPoint]) {
        precondition((0..<24).contains(lane))
        let fraction = (Double(lane) + 0.5) / 24
        let variation = (Double(lane) * 0.618034).truncatingRemainder(dividingBy: 1)
        let finalAngle = -0.4 - fraction * 0.1
        let startAngle = Double.pi + 0.9 - fraction * 0.3
        precondition(startAngle - finalAngle < Double.pi * 2)
        var points: [CGPoint] = []
        for step in 0...128 {
            let angle = startAngle + (finalAngle - startAngle) * Double(step) / 128
            let wall = Self.radius(angle: angle, perimeter: perimeter) - 3 - variation * 8
            let radius = 32 + (wall - 32) * (1 - fraction)
            points.append(CGPoint(x: 183 + cos(angle) * radius, y: 221 + sin(angle) * radius))
        }
        Self.appendExit(to: &points, fraction: fraction)
        var distances = [0.0]
        for index in 1..<points.count {
            distances.append(distances[index - 1] + hypot(points[index].x - points[index - 1].x,
                                                          points[index].y - points[index - 1].y))
        }
        self.points = points
        self.distances = distances
        precondition(length > 0)
    }

    func position(at progress: Double) -> CGPoint {
        let distance = min(1, max(0, progress)) * length
        var lower = 0
        var upper = distances.count - 1
        for _ in 0..<9 {
            if lower + 1 < upper {
                let middle = (lower + upper) / 2
                if distances[middle] < distance { lower = middle } else { upper = middle }
            }
        }
        let fraction = (distance - distances[lower]) / (distances[upper] - distances[lower])
        return CGPoint(x: points[lower].x + (points[upper].x - points[lower].x) * fraction,
                       y: points[lower].y + (points[upper].y - points[lower].y) * fraction)
    }

    private static func radius(angle: Double, perimeter: [CGPoint]) -> Double {
        let direction = CGPoint(x: cos(angle), y: sin(angle))
        var radius = direction.y < 0 ? (145 - 221) / direction.y : Double.infinity
        for index in 0..<perimeter.count {
            let start = perimeter[index]
            let end = perimeter[(index + 1) % perimeter.count]
            let edge = CGPoint(x: end.x - start.x, y: end.y - start.y)
            let relative = CGPoint(x: start.x - 183, y: start.y - 221)
            let cross = direction.x * edge.y - direction.y * edge.x
            if abs(cross) > 1e-8 {
                let distance = (relative.x * edge.y - relative.y * edge.x) / cross
                let fraction = (relative.x * direction.y - relative.y * direction.x) / cross
                if distance > 0 && fraction >= 0 && fraction <= 1 { radius = min(radius, distance) }
            }
        }
        precondition(radius.isFinite)
        return radius
    }

    private static func appendExit(to points: inout [CGPoint], fraction: Double) {
        let end = points[points.count - 1]
        let previous = points[points.count - 2]
        let exitX = 112 + (1 - fraction) * 177
        let ductY = 108.0
        precondition(end.y > ductY)
        let tangent = min(28, (end.y - ductY) * 0.45)
        let heading = hypot(end.x - previous.x, end.y - previous.y)
        precondition(heading > 0)
        let first = CGPoint(x: end.x + (end.x - previous.x) / heading * tangent,
                            y: end.y + (end.y - previous.y) / heading * tangent)
        let second = CGPoint(x: exitX, y: first.y + (ductY - first.y) * 0.7)
        let duct = CGPoint(x: exitX, y: ductY)
        for step in 1...32 {
            points.append(schematicBezier(start: end, first: first, second: second,
                                           end: duct, progress: Double(step) / 32))
        }
        points.append(CGPoint(x: exitX, y: 33))
    }
}

private func schematicBezier(start: CGPoint, first: CGPoint, second: CGPoint,
                              end: CGPoint, progress: Double) -> CGPoint {
    let inverse = 1 - progress
    return CGPoint(
        x: inverse * inverse * inverse * start.x + 3 * inverse * inverse * progress * first.x
            + 3 * inverse * progress * progress * second.x + progress * progress * progress * end.x,
        y: inverse * inverse * inverse * start.y + 3 * inverse * inverse * progress * first.y
            + 3 * inverse * progress * progress * second.y + progress * progress * progress * end.y)
}
