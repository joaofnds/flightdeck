import Combine
import Foundation

func runDashboardChecks() {
    checks_shared_window_selection()
    checks_available_sample_statistics()
    checks_adaptive_quantity_precision()
    checks_mirrored_inspection_reading()
    checks_continuous_fan_motion()
    checks_outgoing_airflow_routes()
    print("Dashboard selection, readings and schematic motion checks passed")
}

private func checks_shared_window_selection() {
    // Selecting a captured sample affects the shared window without losing the range.
    var window = ReferenceWindow()
    window.select(100)
    precondition(window.lower == 90 && window.upper == 120 && window.selected == 100)
    window.range(lower: 12, upper: 40)
    precondition(window.lower == 12 && window.upper == 40 && window.selected == 40)
    window.select(0)
    precondition(window.selected == 12)
    window.select(120)
    precondition(window.selected == 40)

    // A cell owns its full width, including the second half, until the next boundary.
    window.range(lower: 90, upper: 120)
    for fraction in [0.25 / 31, 0.75 / 31] {
        window.selectCell(fraction: fraction)
        precondition(window.selected == 90)
    }
    window.selectCell(fraction: 1.0 / 31)
    precondition(window.selected == 91)
    window.selectCell(fraction: 1)
    precondition(window.selected == 120)
}

private func checks_available_sample_statistics() {
    // Missing observations are excluded from aggregates; measured zero remains a sample.
    let statistics = ReferenceStatistics(values: [1, nil, 0, 8, nil], selected: 3, range: 0...4)
    precondition(statistics.selected == 8)
    precondition(statistics.mean == 3)
    precondition(statistics.maximum == 8)
    precondition(statistics.count == 3)
    let absent = ReferenceStatistics(values: [nil, nil], selected: -1, range: 0...1)
    precondition(absent.selected == nil)
    precondition(absent.mean == nil)
    precondition(absent.maximum == nil)
    precondition(absent.count == 0)
}

private func checks_adaptive_quantity_precision() {
    // Small positive readings must remain distinguishable from measured zero.
    precondition(DashboardQuantity(value: 0.0001, unit: "W") == DashboardQuantity(value: 0.1, unit: "mW"))
    precondition(DashboardQuantity(value: 0.00001, unit: "MB/s") == DashboardQuantity(value: 10, unit: "B/s"))
    precondition(DashboardQuantity(value: 0, unit: "W")?.unit == "W")
    precondition(dashboardReading(0.023856, unit: "ms", digits: 3) != dashboardReading(0, unit: "ms", digits: 3))
}

private func checks_mirrored_inspection_reading() {
    // Plot mirroring must not negate the source reading shown by legends and Data.
    let line = ReferenceLine(name: "Sent", color: "#5794f2", values: [8], negative: true)
    let panel = ReferencePanel(id: "network", title: "Network", unit: "MB/s", lines: [line])
    precondition(referenceSelectedReading(line: line, panel: panel, index: 0) == "8 MB/s")
}

private func checks_continuous_fan_motion() {
    // Speed changes remain continuous, and stopping does not snap the rotor back.
    let start = Date(timeIntervalSinceReferenceDate: 100)
    var motion = SchematicFanMotion(rpm: 1500)
    motion.set(rpm: 1500, enabled: true, at: start)
    let slowTurns = motion.turns(at: start.addingTimeInterval(1))
    let slowDistance = motion.distance(at: start.addingTimeInterval(1))
    precondition(slowTurns > 0)
    precondition(slowDistance > 0)
    let before = motion.turns(at: start.addingTimeInterval(2))
    let distance = motion.distance(at: start.addingTimeInterval(2))
    motion.set(rpm: 6000, enabled: true, at: start.addingTimeInterval(2))
    precondition(motion.turns(at: start.addingTimeInterval(2)) == before)
    precondition(motion.distance(at: start.addingTimeInterval(2)) == distance)
    precondition(motion.turns(at: start.addingTimeInterval(3)) - before > slowTurns)
    precondition(motion.distance(at: start.addingTimeInterval(3)) - distance > slowDistance)
    motion.set(rpm: 0, enabled: true, at: start.addingTimeInterval(3))
    precondition(motion.turns(at: start.addingTimeInterval(100)) == motion.turns(at: start.addingTimeInterval(3)))
    motion.set(rpm: 6000, enabled: false, at: start.addingTimeInterval(101))
    precondition(motion.distance(at: start.addingTimeInterval(200)) == motion.distance(at: start.addingTimeInterval(101)))
}

private func checks_outgoing_airflow_routes() {
    // Every lane exits the full vent without a sharp turn; measured maximum was below 37°.
    for lane in 0..<24 {
        let route = SchematicAirflowRoute(lane: lane)
        let end = route.position(at: 1)
        precondition(end.y == 33 && end.x > 112 && end.x < 289)
        precondition(route.points.allSatisfy { $0.x.isFinite && $0.y.isFinite })
        precondition(route.position(at: 0) != end)
        for index in 1..<(route.points.count - 1) {
            let incoming = route.points[index] - route.points[index - 1]
            let outgoing = route.points[index + 1] - route.points[index]
            let lengths = hypot(incoming.x, incoming.y) * hypot(outgoing.x, outgoing.y)
            precondition(lengths > 0)
            let cosine = (incoming.x * outgoing.x + incoming.y * outgoing.y) / lengths
            precondition(cosine > cos(Double.pi / 4))
        }
    }
}

@MainActor
func runDashboardProjectionChecks(snapshot: ReferenceSnapshot) {
    let lines = referenceAppHistory(snapshot: snapshot, key: "appCpu", range: 12...40, references: [])
    let names = Set(lines.dropFirst().map(\.name))
    precondition(names.contains("clippy-driver"))
    precondition(names.contains("OrbStack"))
    precondition(names.contains("coreaudiod"))
    precondition(names.contains("usbaudiod"))
    precondition(!names.contains("fseventsd"))
    checks_app_history_decomposition(snapshot: snapshot, lines: lines)
    checks_process_cell_precision(snapshot: snapshot)
    checks_unchanged_inspection_does_not_publish(snapshot: snapshot)
    print("Custom-range membership, decomposition and process-cell checks passed")
}

@MainActor
private func checks_app_history_decomposition(snapshot: ReferenceSnapshot, lines: [ReferenceLine]) {
    // Selected app contributions plus Other retain the independent recorded total.
    let total = lines.compactMap { $0.values[20] }.reduce(0, +)
    precondition(abs(total - (snapshot.totals["appCpu"]?[20] ?? 0)) < 0.000001)

    let short = referenceAppHistory(snapshot: snapshot, key: "appCpu", range: 0...1, references: [])
    let hour = referenceAppHistory(snapshot: snapshot, key: "appCpu", range: 0...120, references: [])
    precondition(Set(short.map(\.name)) != Set(hour.map(\.name)))
}

private func checks_process_cell_precision(snapshot: ReferenceSnapshot) {
    // The captured helper has 1768 bytes/s; its neighbour has measured zero at sample 120.
    let active = ReferenceProcessCells(snapshot: snapshot, app: "Brave Browser",
        process: "Brave Browser Helper", index: 120)
    let zero = ReferenceProcessCells(snapshot: snapshot, app: "Brave Browser",
        process: "Brave Browser", index: 120)
    precondition(active.values[4] != "Unavailable")
    precondition(active.values[4] != zero.values[4])
    precondition(zero.values[4] == dashboardReading(0, unit: "", digits: 2))
}

private func - (left: CGPoint, right: CGPoint) -> CGPoint {
    CGPoint(x: left.x - right.x, y: left.y - right.y)
}

@MainActor
private func checks_unchanged_inspection_does_not_publish(snapshot: ReferenceSnapshot) {
    // Hovering within one sample must not invalidate every dashboard panel repeatedly.
    let model = ReferenceModel(snapshot: snapshot)
    var changes = 0
    let observation = model.objectWillChange.sink { changes += 1 }
    defer { observation.cancel() }
    model.inspect(sample: 120)
    model.inspectCell(fraction: 1)
    model.inspectDiagnostic(report: 29)
    precondition(changes == 0)
    model.inspect(sample: 110)
    model.inspect(sample: 110)
    model.inspectCell(fraction: 20.75 / 31)
    precondition(changes == 1)
    model.inspectCell(fraction: 21.25 / 31)
    model.inspectDiagnostic(report: 0)
    model.inspectDiagnostic(report: 0)
    precondition(changes == 3)
}
