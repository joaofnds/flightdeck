import AppKit
import Foundation
import SwiftUI

@MainActor
func runCPUHistoryPanelChecks() throws {
    try paints_clipped_wall_time_intervals_without_stretching_gaps()
    try keeps_overlapping_run_intervals_consistent_with_exact_hit_testing()
    try keeps_zero_and_unavailable_distinct_between_independent_rows()
    try selects_exact_observations_while_inspection_stays_local()
    try bounds_keyboard_inspection_to_the_current_page()
    try pages_a_partial_final_logical_cpu_domain()
    try renders_a_bounded_native_history_panel()
    try paints_measured_zero_differently_from_unavailable_intervals()
    try paints_low_activity_with_its_independent_semantic_color()
    try paints_moderate_activity_with_its_independent_semantic_color()
    try paints_high_activity_with_its_independent_semantic_color()
    try paints_peak_activity_with_its_independent_semantic_color()
}

@MainActor
private func paints_measured_zero_differently_from_unavailable_intervals() throws {
    // Goal: measured zero paints the heatmap rather than disappearing like missing data.
    // Method: compare only plot pixels across 900 sustained idle and unavailable intervals.
    let idle = try panelPixelHistory(idleMissing: false)
    let missing = try panelPixelHistory(idleMissing: true)

    let difference = panelPlotPixelDifferences(try panelPixelImage(idle),
        try panelPixelImage(missing))

    precondition(difference > 9_000, "Measured zero did not paint the logical CPU row")
}

@MainActor
private func paints_low_activity_with_its_independent_semantic_color() throws {
    // Goal: idle intervals use the low activity semantic color.
    // Method: change that role alone and require sustained changes inside the plot.
    try panelSemanticRolePaints(\.cpuActivityLow, name: "low")
}

@MainActor
private func paints_moderate_activity_with_its_independent_semantic_color() throws {
    // Goal: moderate intervals use their own semantic color.
    // Method: change that role alone and require sustained changes inside the plot.
    try panelSemanticRolePaints(\.cpuActivityModerate, name: "moderate")
}

@MainActor
private func paints_high_activity_with_its_independent_semantic_color() throws {
    // Goal: high intervals use their own semantic color.
    // Method: change that role alone and require sustained changes inside the plot.
    try panelSemanticRolePaints(\.cpuActivityHigh, name: "high")
}

@MainActor
private func paints_peak_activity_with_its_independent_semantic_color() throws {
    // Goal: peak intervals use their own semantic color.
    // Method: change that role alone and require sustained changes inside the plot.
    try panelSemanticRolePaints(\.cpuActivityPeak, name: "peak")
}

@MainActor
private func panelSemanticRolePaints(_ role: WritableKeyPath<FlightdeckTheme, Color>,
                                    name: String) throws {
    let history = try panelPixelHistory(idleMissing: false)
    var changed = FlightdeckTheme()
    changed[keyPath: role] = Color(red: 0.12, green: 0.32, blue: 0.94)

    let difference = panelPlotPixelDifferences(try panelPixelImage(history),
        try panelPixelImage(history, theme: changed))

    precondition(difference > 9_000, "The \(name) semantic role did not color plot intervals")
}

@MainActor
private func panelPixelImage(_ history: SelectedCPUHistory,
                             theme: FlightdeckTheme = FlightdeckTheme()) throws -> NSBitmapImageRep {
    let renderer = ImageRenderer(content: LogicalCPUHistoryPanel(history: history,
        notice: nil, busy: false, onPage: { _ in
            preconditionFailure("Rendering changed the logical CPU page")
        }, onSelect: { _ in
            preconditionFailure("Rendering changed the recorded selection")
        }).frame(width: 1080).environment(\.flightdeckTheme, theme)
            .foregroundStyle(theme.text).preferredColorScheme(.dark))
    renderer.scale = 1

    guard let image = renderer.cgImage else { preconditionFailure("The panel did not render") }

    return NSBitmapImageRep(cgImage: image)
}

private func panelPlotPixelDifferences(_ first: NSBitmapImageRep,
                                      _ second: NSBitmapImageRep) -> Int {
    precondition(first.pixelsWide == 1080)
    precondition(first.pixelsWide == second.pixelsWide)
    precondition(first.pixelsHigh == second.pixelsHigh)
    precondition(first.pixelsHigh > 200)
    var differences = 0

    // This interior crop excludes CPU labels, controls, axis text, readout and legend.
    for y in 90..<(first.pixelsHigh - 100) {
        for x in 90..<1000 {
            guard let source = first.colorAt(x: x, y: y)?.usingColorSpace(.sRGB),
                  let target = second.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else {
                preconditionFailure("The rendered plot has no RGB pixel")
            }

            let distance = abs(source.redComponent - target.redComponent)
                + abs(source.greenComponent - target.greenComponent)
                + abs(source.blueComponent - target.blueComponent)

            if distance > 0.06 { differences += 1 }
        }
    }

    return differences
}

private func panelPixelHistory(idleMissing: Bool) throws -> SelectedCPUHistory {
    let points = (0..<900).map { index in
        panelObservation(9_007_199_254_740_993 + UInt64(index),
            wallNs: (1_000 + UInt64(index)) * 1_000_000_000 + 100_000_000)
    }
    let current = points[899]
    let dictionary: (Observation) -> [String: Any] = { point in
        ["run": point.run, "boot": point.boot, "publication": point.publication,
         "run_tick": point.runTick, "wall_ns": point.wallNs, "segment": UInt64(1)]
    }
    let data = try JSONSerialization.data(withJSONObject: ["value": [
        "second": UInt64(1899), "selected": dictionary(current),
        "alternatives": [dictionary(current)], "trends": points.map(dictionary),
        "context_seconds": UInt64(900)]])
    let selection: Selection = try decodeRecordingResponse(data)
    let baseline = panelObservation(points[0].publication - 1, wallNs: 999_100_000_000)
    let samples = points.enumerated().map { index, point in
        CPUHistoryObservation(observation: point,
            previousObservation: index == 0 ? baseline : points[index - 1],
            intervalAvailable: true, cpuCount: 4)
    }
    let values: [Double?] = [idleMissing ? nil : 0, 65, 85, 95]
    let rows = values.enumerated().map { row, value in
        LogicalCPUHistoryRow(cpu: UInt32(row), busyPercent: Array(repeating: value, count: 900))
    }
    let payload = CPUHistoryPayload(observation: current, contextSeconds: 900,
        firstNs: 1_000_000_000_000, endNs: 1_900_000_000_000, offset: 0,
        total: 4, retentionLimited: false, observations: samples, cpus: rows)

    return try SelectedCPUHistory(payload: payload, selection: selection)
}

private func keeps_overlapping_run_intervals_consistent_with_exact_hit_testing() throws {
    // Goal: a newer run's overlapping interval owns the same pixels that hit testing selects.
    // Method: overlap two valid independent run intervals by fifty milliseconds.
    let history = try panelHistory(overlap: true)

    let geometry = CPUHistoryGeometry(history: history)
    let overlap = geometry.inspect(row: 0, fraction: 0.225 / 900)

    precondition(geometry.intervals[0].endNs == 1_000_200_000_000)
    precondition(geometry.intervals[0].firstNs == 1_000_000_000_000)
    precondition(geometry.intervals[1].firstNs == 1_000_200_000_000)
    precondition(overlap?.observationIndex == 1)
    precondition(overlap.flatMap { geometry.value($0) } == nil)
    precondition(geometry.value(CPUHistoryInspection(row: 1, observationIndex: 1)) == 75)
    precondition(overlap.flatMap { geometry.target($0) }?.identity.run == "overlapping-run")
}

@MainActor
private func renders_a_bounded_native_history_panel() throws {
    // Goal: the native panel renders its Canvas and controls without a web renderer.
    // Method: render a two-row real history DTO at a bounded desktop width.
    let history = try panelHistory()
    let renderer = ImageRenderer(content: LogicalCPUHistoryPanel(history: history,
        notice: nil, busy: false, onPage: { _ in
            preconditionFailure("Rendering changed the logical CPU page")
        }, onSelect: { _ in
            preconditionFailure("Rendering changed the recorded selection")
        }).frame(width: 780).foregroundStyle(FlightdeckTheme().text)
            .preferredColorScheme(.dark))

    let image = renderer.cgImage

    precondition(image != nil)
    precondition(image?.width == 780)
    precondition((image?.height ?? 0) > 100)
}

private func keeps_zero_and_unavailable_distinct_between_independent_rows() throws {
    // Goal: a measured idle CPU remains distinct from a missing measurement.
    // Method: inspect the same interval in two independent logical CPU rows.
    let geometry = CPUHistoryGeometry(history: try panelHistory())

    let idle = CPUHistoryInspection(row: 0, observationIndex: 0)
    let missing = CPUHistoryInspection(row: 0, observationIndex: 1)
    let busy = CPUHistoryInspection(row: 1, observationIndex: 1)

    precondition(geometry.value(idle) == 0)
    precondition(geometry.value(missing) == nil)
    precondition(geometry.value(busy) == 75)
    precondition(geometry.reading(idle) == "0%")
    precondition(geometry.reading(missing) == "Unavailable")
}

private func selects_exact_observations_while_inspection_stays_local() throws {
    // Goal: a click target preserves identities larger than a Double can represent exactly.
    // Method: hit the first painted interval and compare its complete selection target.
    let history = try panelHistory()
    let geometry = CPUHistoryGeometry(history: history)
    let inspected = geometry.inspect(row: 0, fraction: 0.1 / 900)

    let target = inspected.flatMap { geometry.target($0) }

    precondition(target == RecordedSelectionTarget(second: 1000,
        identity: RecordedObservationIdentity(run: "panel-run", boot: "panel-boot",
            publication: 9_007_199_254_740_993)))
    precondition(history.observation.publication == 9_007_199_254_740_997)
    precondition(geometry.selected?.observationIndex == 3)
    precondition(abs(geometry.selectedFraction - (899.9 / 900)) < 0.000_000_001)
}

private func bounds_keyboard_inspection_to_the_current_page() throws {
    // Goal: arrows inspect locally without stepping beyond either dimension.
    // Method: start at the exact selection and move against each page boundary.
    let geometry = CPUHistoryGeometry(history: try panelHistory())

    let previous = geometry.step(from: geometry.selected, observations: -1, rows: 0)
    let first = CPUHistoryInspection(row: 0, observationIndex: 0)
    let last = CPUHistoryInspection(row: 1, observationIndex: 3)

    precondition(previous == CPUHistoryInspection(row: 0, observationIndex: 2))
    precondition(geometry.step(from: first, observations: -1, rows: -1) == first)
    precondition(geometry.step(from: last, observations: 1, rows: 1) == last)
    precondition(geometry.inspect(row: -1, fraction: 0.1) == nil)
    precondition(geometry.inspect(row: 2, fraction: 0.1) == nil)
    precondition(geometry.inspect(row: 0, fraction: .nan) == nil)
}

private func pages_a_partial_final_logical_cpu_domain() throws {
    // Goal: page controls name only real logical CPUs in a partial final page.
    // Method: use CPU 32 and 33 as the final page of a 34-CPU domain.
    let first = CPUHistoryGeometry(history: try panelHistory(total: 34))
    let last = CPUHistoryGeometry(history: try panelHistory(offset: 32, total: 34))

    precondition(first.previousPage == nil)
    precondition(first.nextPage == 32)
    precondition(last.previousPage == 0)
    precondition(last.nextPage == nil)
    precondition(last.history.cpus.map(\.cpu) == [32, 33])
}

private func paints_clipped_wall_time_intervals_without_stretching_gaps() throws {
    // Goal: elapsed time, rather than observation count, sets every painted width.
    // Method: inspect unequal intervals, a clipped predecessor and a genuine gap.
    let history = try panelHistory()

    let geometry = CPUHistoryGeometry(history: history)

    precondition(geometry.intervals.map(\.firstNs) == [
        1_000_000_000_000, 1_000_250_000_000, 1_001_800_000_000])
    precondition(geometry.intervals.map(\.endNs) == [
        1_000_250_000_000, 1_001_000_000_000, 1_002_100_000_000])
    precondition(geometry.inspect(row: 0, fraction: 0.1 / 900)?.observationIndex == 0)
    precondition(geometry.inspect(row: 0, fraction: 0.5 / 900)?.observationIndex == 1)
    precondition(geometry.inspect(row: 0, fraction: 1.5 / 900) == nil)
    precondition(geometry.inspect(row: 0, fraction: 900 / 900) == nil)
}

private func panelObservation(_ publication: UInt64, wallNs: UInt64,
                              run: String = "panel-run") -> Observation {
    Observation(run: run, boot: "panel-boot", publication: publication,
        runTick: publication, wallNs: wallNs, windowLosses: nil)
}

private func panelHistory(offset: UInt32 = 0, total: UInt32 = 2,
                          overlap: Bool = false) throws -> SelectedCPUHistory {
    let publication: UInt64 = 9_007_199_254_740_993
    let first = panelObservation(publication, wallNs: 1_000_250_000_000)
    let second = panelObservation(publication + 1, wallNs: 1_001_000_000_000,
        run: overlap ? "overlapping-run" : "panel-run")
    let third = panelObservation(publication + 3, wallNs: 1_002_100_000_000)
    let selected = panelObservation(publication + 4, wallNs: 1_899_900_000_000)
    let observations = [first, second, third, selected]
    let dictionary: (Observation) -> [String: Any] = { point in
        ["run": point.run, "boot": point.boot, "publication": point.publication,
         "run_tick": point.runTick, "wall_ns": point.wallNs, "segment": UInt64(1)]
    }
    let data = try JSONSerialization.data(withJSONObject: ["value": [
        "second": UInt64(1899), "selected": dictionary(selected),
        "alternatives": [dictionary(selected)], "trends": observations.map(dictionary),
        "context_seconds": UInt64(900)]])
    let selection: Selection = try decodeRecordingResponse(data)
    let count = Int(min(SelectedCPUHistory.cpusPerPage, total - offset))
    let rows = (0..<count).map { index in
        LogicalCPUHistoryRow(cpu: offset + UInt32(index),
            busyPercent: index == 0 ? [0, nil, 95, nil] : [50, 75, nil, nil])
    }
    let samples = [
        CPUHistoryObservation(observation: first,
            previousObservation: panelObservation(publication - 1, wallNs: 999_800_000_000),
            intervalAvailable: true, cpuCount: total),
        CPUHistoryObservation(observation: second, previousObservation: overlap
            ? panelObservation(publication, wallNs: 1_000_200_000_000, run: second.run) : first,
            intervalAvailable: true, cpuCount: total),
        CPUHistoryObservation(observation: third,
            previousObservation: panelObservation(publication + 2, wallNs: 1_001_800_000_000),
            intervalAvailable: true, cpuCount: total),
        CPUHistoryObservation(observation: selected, previousObservation: nil,
            intervalAvailable: false, cpuCount: total)]
    let payload = CPUHistoryPayload(observation: selected, contextSeconds: 900,
        firstNs: 1_000_000_000_000, endNs: 1_900_000_000_000, offset: offset,
        total: total, retentionLimited: false, observations: samples, cpus: rows)

    return try SelectedCPUHistory(payload: payload, selection: selection)
}
