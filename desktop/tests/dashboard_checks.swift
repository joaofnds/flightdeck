import AppKit
import Combine
import Foundation
import SwiftUI

@MainActor
func runOverviewInspectionChecks() throws {
    try reads_the_inspected_observation_in_overview_summaries()
    try reads_schematic_fans_and_battery_from_the_inspected_observation()
    preserves_the_active_graph_when_another_graph_exits()
    keeps_the_pointer_at_its_screen_position_when_the_plot_changes()
    clears_the_pointer_when_scrolling_moves_the_plot_away()
    coalesces_pointer_motion_within_one_recorded_second()
    try keeps_missing_inspections_empty_until_the_preview_ends()
    try preserves_inspection_when_the_live_collector_changes_runs()
    retargets_the_pointer_through_the_mounted_chart_modifier()
    try paints_the_installed_capacity_in_the_actual_overview()
    try paints_translucent_memory_in_the_actual_overview()
    scales_small_power_without_rounding_it_to_zero()
    labels_fixed_cpu_capacity_in_twenty_percent_steps()
    labels_installed_memory_in_binary_steps()
    preserves_zero_in_mirrored_traffic_scales()
    retains_explicit_gpu_capacity_above_one_hundred_percent()
    gives_empty_charts_a_finite_scale()
    try expands_the_actual_overview_for_gpu_activity_above_nominal_capacity()
    try paints_gpu_activity_in_the_actual_overview()
    try reads_gpu_activity_in_the_actual_overview()
    try paints_gpu_activity_in_the_actual_gpu_pane()
    try paints_soc_temperature_in_the_actual_energy_pane()
    try separates_gpu_power_in_the_actual_energy_pane()
    try separates_system_power_in_the_actual_energy_pane()
    print("Actual Overview and GPU activity, Energy power panels and SoC history render checks passed")
    try uses_only_selected_schematic_sources()
    try reports_the_inspected_reading_to_accessibility()
    // Keyboard navigation must retain siblings that share a wall second.
    let data = Data(#"""
        {"value":[
          {"run":"first","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1},
          {"run":"second","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":2},
          {"run":"second","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":2}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let selected = Observation(run: "second", boot: "boot", publication: 1, runTick: 1,
        wallNs: 1_000_000_000_000, windowLosses: nil)
    let first = Observation(run: "first", boot: "boot", publication: 1, runTick: 1,
        wallNs: 1_000_000_000_000, windowLosses: nil)
    let repeated = Selection(second: 1000, selected: selected, alternatives: [first, selected],
        summary: points[1], trends: Array(points.prefix(2)), contextSeconds: 900)
    let latest = Observation(run: "second", boot: "boot", publication: 2, runTick: 2,
        wallNs: 1_001_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1001, selected: latest, alternatives: [latest],
        summary: points[2], trends: points, contextSeconds: 900)
    let inspection = ChartInspectionState()

    precondition(inspection.boundary(first: true, in: repeated))
    precondition(inspection.target(in: repeated)?.identity == points[0].identity,
        "Overview Home reconstructed a different observation from the same date")
    precondition(inspection.boundary(first: true, in: selection))
    precondition(inspection.move(step: 1, in: selection))
    precondition(inspection.target(in: selection)?.identity == points[1].identity,
        "Overview Right lost the repeated-second sibling")
    precondition(inspection.move(step: 1, in: selection))
    precondition(inspection.target(in: selection)?.identity == points[2].identity)
    precondition(inspection.move(step: -1, in: selection))
    precondition(inspection.target(in: selection)?.identity == points[1].identity)
    precondition(inspection.boundary(first: false, in: selection))
    precondition(inspection.point(in: selection)?.id == points[2].id)
    inspection.hover(at: Date(timeIntervalSince1970: 1000))
    precondition(inspection.point(in: repeated)?.id == points[1].id)
    inspection.hover(at: Date(timeIntervalSince1970: 999))
    precondition(inspection.point(in: selection) == nil)
    precondition(inspection.date(in: selection) == Date(timeIntervalSince1970: 999))
    inspection.clear()
    precondition(inspection.date(in: selection) == nil)
    precondition(inspection.target(in: selection) == nil)
    print("Overview keyboard inspection preserves repeated-second identities and empty hover")
}

@MainActor
private func reads_the_inspected_observation_in_overview_summaries() throws {
    // Goal: hovering an earlier observation changes the actual summary tiles too.
    // Method: compare only summary-row pixels while keeping the live selection fixed.
    let data = Data(#"""
        {"value":[
          {"run":"hover","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"cpu_percent":10,"pressure":2,"power_watts":5,
           "fan_rpm":[0,1456],
           "storage":{"state":"complete","devices":[],"read_bps":1000000,"write_bps":2000000}},
          {"run":"hover","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"cpu_percent":80,"pressure":0,"power_watts":50,
           "fan_rpm":[5000,6000],
           "storage":{"state":"complete","devices":[],"read_bps":8000000,"write_bps":9000000}}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let selected = Observation(run: "hover", boot: "boot", publication: 2, runTick: 2,
        wallNs: 1_001_000_000_000, windowLosses: nil)
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = Selection(second: 1001, selected: selected, alternatives: [selected],
        summary: points[1], trends: points, contextSeconds: 900)
    let inspection = ChartInspectionState()
    let baseline = scalarPanelImage(Dashboard(inspection: inspection),
        model: model, theme: FlightdeckTheme(), height: 1800)

    inspection.hover(at: Date(timeIntervalSince1970: 1000))
    let inspected = scalarPanelImage(Dashboard(inspection: inspection),
        model: model, theme: FlightdeckTheme(), height: 1800)

    let changed = scalarPanelPixelDifferences(baseline, inspected, rows: 0..<110)
    precondition(changed > 100,
        "Overview kept current summaries while inspecting an earlier observation: \(changed) pixels")
    precondition(model.selection?.selected?.identity == selected.identity)
    precondition(scalarPanelPixelDifferences(baseline, inspected, rows: 1000..<1700) > 100,
        "Overview kept current schematic readings during inspection")
    precondition(scalarPanelPixelDifferences(baseline, inspected,
        rows: 690..<715, columns: 380..<700) > 20,
        "Overview kept current disk footers during inspection")
}

@MainActor
private func preserves_inspection_when_the_live_collector_changes_runs() throws {
    // Goal: a new collector run in the same recording does not end pointer inspection.
    // Method: keep the Dashboard mounted while replacing its selected run.
    let point: Metric = try decodeRecordingResponse(Data(#"""
        {"value":{"run":"first","boot":"boot","publication":1,"wall_ns":1000000000000,
         "segment":1,"cpu_percent":10}}
        """#.utf8))
    let first = Observation(run: "first", boot: "boot", publication: 1, runTick: 1,
        wallNs: point.wallNs, windowLosses: nil)
    let next = Observation(run: "next", boot: "boot", publication: 1, runTick: 1,
        wallNs: point.wallNs, windowLosses: nil)
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = Selection(second: 1000, selected: first, alternatives: [first],
        summary: point, trends: [point], contextSeconds: 900)
    let inspection = ChartInspectionState()
    let renderer = ImageRenderer(content: Dashboard(inspection: inspection)
        .environmentObject(model).frame(width: 1080, height: 1800))
    precondition(renderer.cgImage != nil)
    inspection.hover(at: Date(timeIntervalSince1970: 999))
    precondition(renderer.cgImage != nil)

    model.selection = Selection(second: 1000, selected: next, alternatives: [next],
        summary: nil, trends: [point], contextSeconds: 900)
    renderer.content = Dashboard(inspection: inspection)
        .environmentObject(model).frame(width: 1080, height: 1800)
    precondition(renderer.cgImage != nil)

    precondition(inspection.date(in: model.selection) == Date(timeIntervalSince1970: 999),
        "A live collector run change erased the mounted Overview's inspection")
}

@MainActor
private func retargets_the_pointer_through_the_mounted_chart_modifier() {
    // Goal: plot geometry changes reach inspection through the actual view callbacks.
    // Method: resize a mounted modifier while its screen-space pointer stays fixed.
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    let inspection = ChartInspectionState()
    let owner = UUID()
    let content = Color.clear.modifier(ChartInspection(inspection: inspection,
        domain: 1000...1900, readout: { _ in "Unavailable" }, owner: owner))
        .environmentObject(model)
    let renderer = ImageRenderer(content: content.frame(width: 415, height: 200))
    precondition(renderer.cgImage != nil)
    inspection.hover(at: CGPoint(x: 200, y: 50),
        in: CGRect(x: 0, y: 0, width: 415, height: 200), domain: 1000...1900, owner: owner)
    precondition(renderer.cgImage != nil)

    renderer.content = content.frame(width: 715, height: 200)
    precondition(renderer.cgImage != nil)

    precondition(inspection.date(in: nil) == Date(timeIntervalSince1970: 1225),
        "The mounted chart did not retarget inspection after resizing")
    renderer.content = Color.clear.modifier(ChartInspection(inspection: inspection,
        domain: 1001...1901, readout: { _ in "Unavailable" }, owner: owner))
        .environmentObject(model).frame(width: 715, height: 200)
    precondition(renderer.cgImage != nil)
    precondition(inspection.date(in: nil) == Date(timeIntervalSince1970: 1226),
        "The mounted chart did not retarget inspection after a live-domain advance")
}

@MainActor
private func reads_schematic_fans_and_battery_from_the_inspected_observation() throws {
    // Goal: the schematic uses the hovered publication without joining current fan data.
    // Method: inspect a metric with different readings from the selected fan page.
    let data = Data(#"""
        {"value":[
          {"run":"hover","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"cpu_percent":10,"gpu_active_percent":20,
           "fan_rpm":[0,1456],"memory_total_bytes":51539607552,
           "battery":{"source_continuous_ns":0,"age_seconds":0,
             "charge_percent":40,"power_watts":-8}},
          {"run":"hover","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"cpu_percent":80,"fan_rpm":[5000,6000]}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let observation = Observation(run: "hover", boot: "boot", publication: 2, runTick: 2,
        wallNs: 1_001_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1001, selected: observation, alternatives: [observation],
        summary: points[1], trends: points, contextSeconds: 900)
    let currentFans = SelectedFanReadings(observation: observation, fan0: 5000, fan1: 6000)
    let inspection = ChartInspectionState()

    inspection.hover(at: Date(timeIntervalSince1970: 1000))
    let readings = inspection.schematic(in: selection, fans: currentFans)

    precondition(readings.cpuPercent == 10)
    precondition(readings.gpuPercent == 20)
    precondition(readings.fan0 == 0)
    precondition(readings.fan1 == 1456)
    precondition(readings.memoryTotalGiB == 48)
    precondition(readings.batteryPercent == 40)
    precondition(readings.batteryWatts == -8)
    precondition(readings.cpuTemperature == nil)
    precondition(readings.gpuTemperature == nil)
}

@MainActor
private func preserves_the_active_graph_when_another_graph_exits() {
    // Goal: a late exit from the previous plot cannot erase the next plot's preview.
    // Method: enter two plots, then deliver the first plot's exit last.
    let inspection = ChartInspectionState()
    let first = UUID()
    let second = UUID()
    let frame = CGRect(x: 0, y: 0, width: 400, height: 200)
    inspection.hover(at: CGPoint(x: 100, y: 50), in: frame, domain: 1000...1900, owner: first)
    inspection.hover(at: CGPoint(x: 200, y: 50), in: frame, domain: 1000...1900, owner: second)
    let expected = inspection.date(in: nil)

    inspection.clear(owner: first)

    precondition(expected != nil)
    precondition(inspection.date(in: nil) == expected,
        "An inactive graph cleared another graph's inspection")
}

@MainActor
private func keeps_the_pointer_at_its_screen_position_when_the_plot_changes() {
    // Goal: inspection follows the time beneath a stationary pointer as live charts move.
    // Method: change the domain, then resize the same plot around that screen position.
    let inspection = ChartInspectionState()
    let owner = UUID()
    let frame = CGRect(x: 0, y: 0, width: 415, height: 200)
    inspection.hover(at: CGPoint(x: 200, y: 50), in: frame, domain: 1000...1900, owner: owner)
    precondition(inspection.date(in: nil) == Date(timeIntervalSince1970: 1450))

    inspection.retarget(in: frame, domain: 1001...1901, owner: owner)

    precondition(inspection.date(in: nil) == Date(timeIntervalSince1970: 1451))
    inspection.retarget(in: CGRect(x: 0, y: 0, width: 715, height: 200),
        domain: 1001...1901, owner: owner)
    precondition(inspection.date(in: nil) == Date(timeIntervalSince1970: 1226))
}

@MainActor
private func clears_the_pointer_when_scrolling_moves_the_plot_away() {
    // Goal: a plot no longer under the pointer cannot keep historical page readings active.
    // Method: retain the pointer, then move the plot's global frame beyond it.
    let inspection = ChartInspectionState()
    let owner = UUID()
    let frame = CGRect(x: 0, y: 0, width: 415, height: 200)
    inspection.hover(at: CGPoint(x: 200, y: 50), in: frame, domain: 1000...1900, owner: owner)

    inspection.retarget(in: frame.offsetBy(dx: 0, dy: 100), domain: 1000...1900, owner: owner)

    precondition(inspection.date(in: nil) == nil)
}

@MainActor
private func coalesces_pointer_motion_within_one_recorded_second() {
    // Goal: subsecond pointer motion does not repeatedly invalidate all Overview charts.
    // Method: observe publications while moving within one cell and then across its edge.
    let inspection = ChartInspectionState()
    let owner = UUID()
    var changes = 0
    let subscription = inspection.objectWillChange.sink { changes += 1 }
    let frame = CGRect(x: 0, y: 0, width: 1015, height: 200)

    inspection.hover(at: CGPoint(x: 200.1, y: 50), in: frame, domain: 1000...1900, owner: owner)
    inspection.hover(at: CGPoint(x: 200.2, y: 50), in: frame, domain: 1000...1900, owner: owner)
    inspection.hover(at: CGPoint(x: 201.1, y: 50), in: frame, domain: 1000...1900, owner: owner)

    precondition(changes == 2, "Subsecond pointer motion republished the whole page: \(changes)")
    withExtendedLifetime(subscription) {}
}

@MainActor
private func keeps_missing_inspections_empty_until_the_preview_ends() throws {
    // Goal: an empty inspected second never falls back to current values or current fans.
    // Method: inspect a gap in a nonempty selection, then clear the preview.
    let point: Metric = try decodeRecordingResponse(Data(#"""
        {"value":{"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
         "segment":1,"cpu_percent":80,"fan_rpm":[5000,6000]}}
        """#.utf8))
    let observation = Observation(run: "run", boot: "boot", publication: 1, runTick: 1,
        wallNs: point.wallNs, windowLosses: nil)
    let selection = Selection(second: 1000, selected: observation, alternatives: [observation],
        summary: point, trends: [point], contextSeconds: 900)
    let inspection = ChartInspectionState()

    inspection.hover(at: Date(timeIntervalSince1970: 999))

    precondition(inspection.summary(in: selection) == nil)
    precondition(inspection.schematic(in: selection, fans: nil).cpuPercent == nil)
    precondition(inspection.schematic(in: selection, fans: nil).fan0 == nil)
    precondition(inspection.reading(in: selection, readout: { "\($0.cpuPercent!)" })
        .hasSuffix("No recorded observation"))
    inspection.clear()
    precondition(inspection.summary(in: selection)?.id == point.id)
    precondition(inspection.date(in: selection) == nil)
}

private func scales_small_power_without_rounding_it_to_zero() {
    // Goal: milliwatt App histories remain visible instead of sharing a one-watt range.
    // Method: scale a 1.4 mW peak and compare its nonzero tick with the zero label.
    let axis = TimeSeriesAxis(range: 0...0.0014, unit: "W", minimum: 0)

    precondition(abs(axis.domain.upperBound - 0.0015) < 1e-15)
    precondition(axis.unit == "mW")
    precondition(axis.label(0.0005) == 0.5.formatted(.number))
    precondition(axis.ticks.count <= 8)
}

private func labels_fixed_cpu_capacity_in_twenty_percent_steps() {
    // Goal: a fixed CPU scale stays at 100% with simple, evenly spaced labels.
    // Method: supply partial activity with the explicit capacity bounds.
    let axis = TimeSeriesAxis(range: 0...37.5, unit: "%", minimum: 0, maximum: 100)

    precondition(axis.domain == 0...100)
    precondition(axis.ticks == [0, 20, 40, 60, 80, 100])
}

private func labels_installed_memory_in_binary_steps() {
    // Goal: the installed-memory reference fits a readable 48 GiB scale.
    // Method: scale that capacity without a caller-supplied upper bound.
    let axis = TimeSeriesAxis(range: 0...48, unit: "GiB")

    precondition(axis.domain == 0...48)
    precondition(axis.ticks == [0, 16, 32, 48])
    precondition(axis.unit == "GiB")
}

private func preserves_zero_in_mirrored_traffic_scales() {
    // Goal: upload and download remain on opposite sides of a labeled zero.
    // Method: scale unequal positive and negative traffic peaks.
    let axis = TimeSeriesAxis(range: -7000...6800, unit: "KiB/s")

    precondition(axis.domain == -10_000...10_000)
    precondition(axis.ticks == [-10_000, -5000, 0, 5000, 10_000])
}

private func retains_explicit_gpu_capacity_above_one_hundred_percent() {
    // Goal: measured GPU values above nominal capacity remain within explicit bounds.
    // Method: pass the existing expanded GPU range through the rendered-axis model.
    let axis = TimeSeriesAxis(range: 0...150, unit: "%", minimum: 0, maximum: 150)

    precondition(axis.domain == 0...150)
    precondition(axis.ticks == [0, 50, 100, 150])
}

private func gives_empty_charts_a_finite_scale() {
    // Goal: an empty or all-zero chart retains a nondegenerate scale.
    // Method: scale the same zero extent produced by an unavailable series.
    let axis = TimeSeriesAxis(range: 0...0, unit: "W")

    precondition(axis.domain == 0...1)
    precondition(axis.ticks.first == 0)
    precondition(axis.ticks.last == 1)
    precondition(axis.ticks.allSatisfy(\.isFinite))
}

@MainActor
private func paints_the_installed_capacity_in_the_actual_overview() throws {
    // Goal: the installed-capacity line paints inside the actual memory plot.
    // Method: count its neutral pixels above the nonzero category stack, away from labels.
    let image = try overviewMemoryImage()
    let reference = NSColor(FlightdeckTheme().muted).usingColorSpace(.sRGB)!

    let count = overviewMemoryPixels(image, rows: 180..<220) { color in
        abs(color.redComponent - reference.redComponent)
            + abs(color.greenComponent - reference.greenComponent)
            + abs(color.blueComponent - reference.blueComponent) < 0.14
    }

    precondition(count > 30, "Overview omitted its installed-memory reference: \(count) pixels")
}

@MainActor
private func paints_translucent_memory_in_the_actual_overview() throws {
    // Goal: category interiors remain translucent instead of hiding the panel behind them.
    // Method: paint Active white and inspect only its interior, away from lines and labels.
    let image = try overviewMemoryImage()

    let count = overviewMemoryPixels(image, rows: 315..<330) { color in
        let brightness = (color.redComponent + color.greenComponent + color.blueComponent) / 3

        return brightness > 0.18 && brightness < 0.7
    }

    precondition(count > 100, "Overview memory lost its translucent interior: \(count) pixels")
}

@MainActor
private func overviewMemoryImage() throws -> NSBitmapImageRep {
    let values: [[String: Any]] = [1_000_000_000_000, 1_899_000_000_000].enumerated().map {
        ["run": "memory", "boot": "boot", "publication": $0.offset + 1,
         "wall_ns": $0.element, "segment": 1, "memory_total_bytes": 51_539_607_552,
         "memory_active_bytes": 8_589_934_592, "memory_wired_bytes": 4_294_967_296,
         "memory_compressed_bytes": 4_294_967_296]
    }
    let points: [Metric] = try decodeRecordingResponse(
        JSONSerialization.data(withJSONObject: ["value": values]))
    let selected = Observation(run: "memory", boot: "boot", publication: 2, runTick: 2,
        wallNs: 1_899_000_000_000, windowLosses: nil)
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = Selection(second: 1899, selected: selected, alternatives: [selected],
        summary: points[1], trends: points, contextSeconds: 900)
    var theme = FlightdeckTheme()
    theme.memoryActiveSeries = .white

    return scalarPanelImage(Dashboard(), model: model, theme: theme, height: 1800)
}

private func overviewMemoryPixels(_ image: NSBitmapImageRep, rows: Range<Int>,
                                  matching: (NSColor) -> Bool) -> Int {
    precondition(image.pixelsWide == 1080)
    precondition(image.pixelsHigh == 1800)
    var count = 0
    for row in rows {
        for column in 820..<960 {
            guard let color = image.colorAt(x: column, y: row)?.usingColorSpace(.sRGB) else {
                preconditionFailure("The memory plot has no RGB pixel")
            }

            if matching(color) { count += 1 }
        }
    }

    return count
}

private func uses_only_selected_schematic_sources() throws {
    // Goal: the schematic combines only the selected publication's measured sources.
    // Method: decode independent values and substitute same-second run/boot/publication siblings.
    let data = Data(#"""
        {"value":{"run":"run","boot":"boot","publication":2,"wall_ns":1000000000000,
          "segment":1,"cpu_percent":15.5,"gpu_active_percent":27.5,
          "memory_total_bytes":51539607552,"memory_active_bytes":1234,"soc_celsius":99}}
        """#.utf8)
    let point: Metric = try decodeRecordingResponse(data)
    let observation = Observation(run: "run", boot: "boot", publication: 2, runTick: 1,
        wallNs: 1_000_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1000, selected: observation, alternatives: [observation],
        summary: point, trends: [point], contextSeconds: 900)
    let fans = SelectedFanReadings(observation: observation, fan0: 0, fan1: 1456)

    let readings = schematicReadings(selection: selection, fans: fans)

    precondition(readings.cpuPercent == 15.5)
    precondition(readings.gpuPercent == 27.5)
    precondition(readings.memoryTotalGiB == 48)
    precondition(readings.fan0 == 0)
    precondition(readings.fan1 == 1456)
    precondition(readings.cpuTemperature == nil)
    precondition(readings.gpuTemperature == nil)
    precondition(readings.memoryUsedGiB == nil)
    precondition(readings.memoryTemperature == nil)
    precondition(readings.batteryPercent == nil)
    precondition(readings.batteryWatts == nil)
    precondition(readings.thermalState == nil)
    let absent = schematicReadings(selection: selection, fans: nil)
    precondition(absent.fan0 == nil)
    precondition(absent.fan1 == nil)
    refuses_schematic_sibling(selection,
        identity: RecordedObservationIdentity(run: "other", boot: "boot", publication: 2))
    refuses_schematic_sibling(selection,
        identity: RecordedObservationIdentity(run: "run", boot: "other", publication: 2))
    refuses_schematic_sibling(selection,
        identity: RecordedObservationIdentity(run: "run", boot: "boot", publication: 1))
    let gap = Selection(second: 1000, selected: nil, alternatives: [], summary: point,
        trends: [point], contextSeconds: 900)
    let missing = schematicReadings(selection: gap, fans: fans)
    precondition(missing.fan0 == nil)
    precondition(missing.cpuPercent == nil)
}

private func refuses_schematic_sibling(_ selection: Selection,
                                       identity: RecordedObservationIdentity) {
    guard let selected = selection.selected, let summary = selection.summary else {
        preconditionFailure("The identity fixture needs a selected summary")
    }

    let sibling = Observation(run: identity.run, boot: identity.boot,
        publication: identity.publication, runTick: selected.runTick,
        wallNs: selected.wallNs, windowLosses: nil)
    let other = Selection(second: selection.second, selected: sibling, alternatives: [sibling],
        summary: summary, trends: selection.trends, contextSeconds: 900)

    let mismatched = schematicReadings(selection: selection,
        fans: SelectedFanReadings(observation: sibling, fan0: 5000, fan1: 6000))
    let invalid = schematicReadings(selection: other, fans: nil)

    precondition(mismatched.fan0 == nil)
    precondition(mismatched.fan1 == nil)
    precondition(invalid.cpuPercent == nil)
    precondition(invalid.gpuPercent == nil)
    precondition(invalid.memoryTotalGiB == nil)
}

func runRecordedComponentChecks() throws {
    try preserves_soc_temperature_values_units_and_exact_selection()
    try uses_independent_gpu_activity_and_temperature_roles()
    try checks_recorded_component_projections()
    try checks_recorded_network_packet_and_error_projections()
    try expands_gpu_activity_for_measured_values_above_nominal_capacity()
    try checks_recorded_alternative_targets()
    // Optional GPU activity distinguishes measured zero from missing source intervals.
    let data = Data(#"""
        {"value":[
          {"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"gpu_active_percent":37.5},
          {"run":"run","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"gpu_active_percent":0},
          {"run":"run","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":1,"gpu_active_percent":null},
          {"run":"run","boot":"boot","publication":4,"wall_ns":1003000000000,
           "segment":1}]}
        """#.utf8)

    let points: [Metric] = try decodeRecordingResponse(data)

    precondition(points.map(\.gpuActivePercent) == [37.5, 0, nil, nil])
    print("GPU activity decoding preserves positive, zero and unavailable readings")
}

private func checks_recorded_network_packet_and_error_projections() throws {
    // Goal: real packet/error units, gaps, identities and non-color cues reach both charts.
    // Method: decode positive, zero, explicit-null and absent readings across two runs.
    let data = Data(#"""
        {"value":[
          {"run":"first","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"network_rx_packets_per_second":4,"network_tx_packets_per_second":6,
           "network_rx_errors_per_second":1,"network_tx_errors_per_second":2},
          {"run":"first","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"network_rx_packets_per_second":0,"network_tx_packets_per_second":0,
           "network_rx_errors_per_second":0,"network_tx_errors_per_second":0},
          {"run":"first","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":1,"network_rx_packets_per_second":null,"network_tx_packets_per_second":null,
           "network_rx_errors_per_second":null,"network_tx_errors_per_second":null},
          {"run":"second","boot":"boot","publication":1,"wall_ns":1003000000000,
           "segment":1}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let selected = Observation(run: "first", boot: "boot", publication: 1, runTick: 0,
        wallNs: 1_000_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1003, selected: selected, alternatives: [selected],
        summary: points[0], trends: points, contextSeconds: 900)
    var theme = FlightdeckTheme()
    theme.networkReceivedPacketsSeries = .pink
    theme.networkSentPacketsSeries = .cyan
    theme.networkReceiveErrorsSeries = .yellow
    theme.networkSendErrorsSeries = .white

    let component = RecordedComponentData(context: RecordedChartContext(selection: selection),
        theme: theme)

    precondition(component.networkPackets.map(\.values) == [[4, 0, nil, nil], [6, 0, nil, nil]])
    precondition(component.networkErrors.map(\.values) == [[1, 0, nil, nil], [2, 0, nil, nil]])
    precondition(component.networkPackets.map(\.unit) == ["packets/s", "packets/s"])
    precondition(component.networkErrors.map(\.unit) == ["errors/s", "errors/s"])
    precondition(component.networkPackets.map(\.name) == ["Received", "Sent"])
    precondition(component.networkErrors.map(\.name) == ["Receive errors", "Send errors"])
    precondition(component.networkPackets.map(\.strokePattern) == [.solid, .dashed])
    precondition(component.networkErrors.map(\.strokePattern) == [.solid, .dashed])
    precondition(component.networkPackets[1].negative == true)
    precondition(component.networkErrors.allSatisfy { $0.negative != true })
    precondition(component.networkPackets.map(\.color)
        == [theme.networkReceivedPacketsSeries, theme.networkSentPacketsSeries])
    precondition(component.networkErrors.map(\.color)
        == [theme.networkReceiveErrorsSeries, theme.networkSendErrorsSeries])
    precondition(component.context.domain == 104...1004)
    precondition(component.context.selectedID == points[0].id)
    precondition(component.context.plot(lines: component.networkPackets)?.segments(line: 0) == [[0, 1]])
    precondition(component.context.plot(lines: component.networkErrors)?.segments(line: 1) == [[0, 1]])
}

private func expands_gpu_activity_for_measured_values_above_nominal_capacity() throws {
    // The source permits activity above 100%; its full recorded value must fit the plot.
    let data = Data(#"""
        {"value":[
          {"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"gpu_active_percent":0},
          {"run":"run","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"gpu_active_percent":100},
          {"run":"run","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":1,"gpu_active_percent":150},
          {"run":"run","boot":"boot","publication":4,"wall_ns":1002000000000,
           "segment":2}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let observation = Observation(run: "run", boot: "boot", publication: 3, runTick: 2,
        wallNs: 1_002_000_000_000, windowLosses: nil)
    let unavailable = Observation(run: "run", boot: "boot", publication: 4, runTick: 3,
        wallNs: 1_002_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1002, selected: observation,
        alternatives: [observation, unavailable],
        summary: points[2], trends: points, contextSeconds: 900)

    let component = RecordedComponentData(context: RecordedChartContext(selection: selection),
        theme: FlightdeckTheme())
    let plot = component.context.plot(lines: [component.gpuActivity])!
    precondition(component.gpuActivity.values == [0, 100, 150, nil])
    precondition(component.gpuActivityStyle.minimum == 0)
    precondition(component.gpuActivityStyle.maximum == 150, "Measured GPU 150% exceeds its axis")
    precondition(component.gpuActivityStyle.isValid)
    precondition(plot.segments(line: 0) == [[0, 1, 2]])
    precondition(dashboardReading(component.gpuActivity.values[2], unit: "%") == "150 %")
}

private func checks_recorded_component_projections() throws {
    // Source units, measured zero and unavailable intervals survive every component projection.
    let data = Data(#"""
        {"value":[
          {"run":"first","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"cpu_percent":25,"gpu_active_percent":37.5,"gpu_watts":2,
           "power_watts":12,"network_rx_bps":2048,"network_tx_bps":1024},
          {"run":"first","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"cpu_percent":0,"gpu_active_percent":0,"gpu_watts":0,
           "power_watts":0,"network_rx_bps":0,"network_tx_bps":0},
          {"run":"first","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":1},
          {"run":"second","boot":"boot","publication":1,"wall_ns":1003000000000,
           "segment":1,"cpu_percent":50,"gpu_active_percent":75,"gpu_watts":4,
           "power_watts":16,"network_rx_bps":4096,"network_tx_bps":2048}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let observation = Observation(run: "second", boot: "boot", publication: 1, runTick: 0,
        wallNs: 1_003_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1003, selected: observation, alternatives: [observation],
        summary: points[3], trends: points, contextSeconds: 900)
    let context = RecordedChartContext(selection: selection)

    let component = RecordedComponentData(context: context, theme: FlightdeckTheme())

    precondition(component.cpu.values == [25, 0, nil, 50])
    precondition(component.gpuActivity.values == [37.5, 0, nil, 75])
    precondition(component.gpuActivityStyle.minimum == 0)
    precondition(component.gpuActivityStyle.maximum == 100)
    precondition(component.energy.map(\.values) == [[12, 0, nil, 16], [2, 0, nil, 4]])
    precondition(component.energy.map(\.unit) == ["W", "W"])
    precondition(component.network.map(\.values) == [[2, 0, nil, 4], [1, 0, nil, 2]])
    precondition(component.network[1].negative == true)
    precondition(component.network[1].values[3] == 2)
    precondition(component.network.map(\.unit) == ["KiB/s", "KiB/s"])
    precondition(context.plot(lines: [component.cpu])?.segments(line: 0) == [[0, 1], [3]])
    precondition(context.selectedID == points[3].id)
    precondition(context.domain == 104...1004)
}

private func checks_recorded_alternative_targets() throws {
    // Explicit alternatives, including repeated seconds, retain their full identity and order.
    let data = Data(#"""
        {"value":[
          {"run":"first","boot":"boot","publication":9,"wall_ns":1000000000000,
           "segment":1,"cpu_percent":10},
          {"run":"second","boot":"other","publication":9,"wall_ns":1000000000000,
           "segment":2,"cpu_percent":20}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let first = Observation(run: "first", boot: "boot", publication: 9, runTick: 8,
        wallNs: 1_000_000_000_000, windowLosses: nil)
    let latest = Observation(run: "second", boot: "other", publication: 9, runTick: 8,
        wallNs: 1_000_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1000, selected: latest, alternatives: [latest, first],
        summary: points[1], trends: points, contextSeconds: 900)

    let context = RecordedChartContext(selection: selection)

    let latestAlternative = try recordedAlternative(selection: selection, target: latest.identity)
    let firstAlternative = try recordedAlternative(selection: selection, target: first.identity)

    precondition(context.target(id: points[1].id) == RecordedSelectionTarget(second: 1000,
        identity: latest.identity))
    precondition(context.target(id: points[0].id) == RecordedSelectionTarget(second: 1000,
        identity: first.identity))
    precondition(latestAlternative == 0)
    precondition(firstAlternative == 1)
    precondition(context.target(id: "unknown") == nil)
    precondition(context.point(at: 1000)?.id == points[1].id)
    precondition(context.point(at: 999) == nil)
}

func runDashboardChecks() {
    preserves_default_curved_plot_geometry()
    preserves_linear_plot_geometry()
    maps_inspection_to_the_shared_plot_frame()
    checks_recorded_plot_boundaries()
    checks_recorded_plot_clock_boundaries()
    checks_recorded_plot_validation()
    checks_shared_window_selection()
    checks_available_sample_statistics()
    checks_adaptive_quantity_precision()
    checks_mirrored_inspection_reading()
    checks_continuous_fan_motion()
    checks_outgoing_airflow_routes()
    preserves_partial_schematic_readouts()
    print("Dashboard selection, readings, schematic motion and bounded time-series checks passed")
}

private func preserves_partial_schematic_readouts() {
    // Goal: useful measured activity and installed memory survive unavailable temperatures.
    // Method: inspect literal partial readings without deriving missing source measurements.
    let readings = SchematicReadings(cpuPercent: 15.5, gpuPercent: 27.5, memoryTotalGiB: 48)
    let decimal = Locale.current.decimalSeparator ?? "."

    precondition(SchematicCallout.cpu.values(readings).primary == "15\(decimal)5%")
    precondition(SchematicCallout.gpu.values(readings).primary == "27\(decimal)5%")
    precondition(SchematicCallout.cpu.values(readings).secondary == "Temperature unavailable")
    precondition(SchematicCallout.gpu.values(readings).secondary == "Temperature unavailable")
    precondition(SchematicCallout.memory.values(readings).primary == "48 GiB installed")
    precondition(SchematicCallout.memory.values(readings).secondary == "Temperature unavailable")
    let empty = SchematicReadings()
    precondition(SchematicCallout.cpu.values(empty).primary == "Unavailable")
    precondition(SchematicCallout.memory.values(empty).primary == "Unavailable")
    let invalid = SchematicReadings(memoryTotalGiB: -1)
    precondition(SchematicCallout.memory.values(invalid).primary == "Unavailable")
    let complete = SchematicReadings(cpuTemperature: 63.4, gpuTemperature: 52.5,
        cpuPercent: 15.5, gpuPercent: 27.5, memoryUsedGiB: 31.7, memoryTotalGiB: 48,
        memoryTemperature: 57.2)
    precondition(SchematicCallout.cpu.values(complete).primary == "63\(decimal)4 °C")
    precondition(SchematicCallout.gpu.values(complete).primary == "52\(decimal)5 °C")
    precondition(SchematicCallout.memory.values(complete).primary == "31\(decimal)7 / 48 GiB")
}

private func checks_recorded_plot_clock_boundaries() {
    // Equal and backward clocks retain observations while breaking interpolation.
    let samples = [10.0, 11, 11, 9, 10].enumerated().map {
        TimeSeriesSample(id: "run:\($0.offset)", time: $0.element, segment: "same")
    }
    let line = TimeSeriesLine(name: "Active", color: .blue, values: [1, 2, 3, 4, 5])

    let plot = TimeSeriesModel(samples: samples, lines: [line], domain: 9...12)

    precondition(plot?.segments(line: 0) == [[0, 1], [2], [3, 4]])
    precondition(plot?.selectedTime(id: "run:2") == 11)
}

private func checks_recorded_plot_validation() {
    // Invalid data is refused; the current maximum context remains representable.
    let samples = (0..<4096).map {
        TimeSeriesSample(id: "run:\($0)", time: Double($0), segment: "same")
    }
    let valid = TimeSeriesLine(name: "Active", color: .blue,
        values: Array(repeating: 0, count: 4096))
    precondition(TimeSeriesModel(samples: samples, lines: [valid], domain: 0...4096) != nil)
    precondition(TimeSeriesModel(samples: samples + [TimeSeriesSample(id: "extra", time: 4096,
        segment: "same")], lines: [], domain: 0...4096) == nil)
    precondition(TimeSeriesModel(samples: [samples[0], samples[0]], lines: [],
        domain: 0...1) == nil)
    precondition(TimeSeriesModel(samples: [samples[0]], lines: [TimeSeriesLine(name: "bad",
        color: .blue, values: [.infinity])], domain: 0...1) == nil)
    precondition(TimeSeriesModel(samples: [samples[0]], lines: [TimeSeriesLine(name: "overflow",
        color: .blue, values: [.greatestFiniteMagnitude])], domain: 0...1) == nil)
    precondition(TimeSeriesModel(samples: [TimeSeriesSample(id: "far", time: -.greatestFiniteMagnitude,
        segment: "same")], lines: [], domain: 0...1) == nil)
    precondition(TimeSeriesModel(samples: [], lines: [],
        domain: -Double.greatestFiniteMagnitude...Double.greatestFiniteMagnitude) == nil)
    precondition(!TimeSeriesStyle(unit: "GiB", minimum: 2, maximum: 1).isValid)
    precondition(!TimeSeriesStyle(unit: "GiB", maximum: .infinity).isValid)
}

private func checks_recorded_plot_boundaries() {
    // Missing readings and changed segments never join across recorded observations.
    let samples = [
        TimeSeriesSample(id: "a", time: 10, segment: "first"),
        TimeSeriesSample(id: "b", time: 11, segment: "first"),
        TimeSeriesSample(id: "c", time: 12, segment: "first"),
        TimeSeriesSample(id: "d", time: 13, segment: "second")
    ]
    let line = TimeSeriesLine(name: "Active", color: .blue, values: [1, nil, 3, 4])

    let plot = TimeSeriesModel(samples: samples, lines: [line], domain: 10...13)

    precondition(plot?.segments(line: 0) == [[0], [2], [3]])
    precondition(plot?.selectedTime(id: "c") == 12)
    precondition(plot?.selectedTime(id: "another-run:c") == nil)
}

func runRecordedMemoryChecks() throws {
    try preserves_independent_swap_amounts_and_memory_rates()
    try uses_independent_swap_and_rate_theme_roles()
    try runRecordedMemoryBoundaryChecks()
    try runRecordedMemoryThemeChecks()
    // Raw VM categories retain their meaning and missing samples remain unavailable.
    let data = Data(#"""
        {"value":[
          {"run":"run","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"memory_active_bytes":3,"memory_wired_bytes":6,
           "memory_compressed_bytes":7,"memory_total_bytes":1,"swap_used_bytes":12,
           "pressure":1},
          {"run":"run","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":2}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let observation = Observation(run: "run", boot: "boot", publication: 2, runTick: 1,
        wallNs: 1_001_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1001, selected: observation, alternatives: [observation],
        summary: points[0], trends: points, contextSeconds: 900)

    let memory = RecordedMemoryData(selection: selection, theme: FlightdeckTheme())

    precondition(memory.categories.map(\.name) == ["Active", "Wired", "Compressed"])
    precondition(memory.categories.map(\.values) == [
        [3.0 / 1_073_741_824, nil], [6.0 / 1_073_741_824, nil], [7.0 / 1_073_741_824, nil]])
    precondition(memory.swap.values == [12.0 / 1_073_741_824, nil])
    precondition(memory.selectedID == points[0].id)
    precondition(memory.pressureText == "Warning")
    let gap = Selection(second: 1002, selected: nil, alternatives: [], summary: nil,
        trends: points, contextSeconds: 900)
    precondition(RecordedMemoryData(selection: gap, theme: FlightdeckTheme()).selectedID == nil)
    precondition(RecordedMemoryData(selection: gap, theme: FlightdeckTheme()).pressureText == "Unavailable")
    print("Recorded memory categories and missing-selection checks passed")
}

private func preserves_independent_swap_amounts_and_memory_rates() throws {
    // Goal: live source amounts and per-second rates retain distinct values, zero and absence.
    // Method: decode independent literals, then inspect the same shared plot and identity adapters.
    let data = Data(#"""
        {"value":[
          {"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"swap_used_bytes":3221225472,"swap_total_bytes":11811160064,
           "memory_pageins_per_second":13.25,"memory_compressions_per_second":29.5,
           "memory_decompressions_per_second":7.125},
          {"run":"run","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"swap_used_bytes":0,"swap_total_bytes":0,
           "memory_pageins_per_second":0,"memory_compressions_per_second":0,
           "memory_decompressions_per_second":0},
          {"run":"run","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":1,"swap_used_bytes":null,"swap_total_bytes":null,
           "memory_pageins_per_second":null,"memory_compressions_per_second":null,
           "memory_decompressions_per_second":null},
          {"run":"run","boot":"boot","publication":4,"wall_ns":1003000000000,
           "segment":1}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let observation = Observation(run: "run", boot: "boot", publication: 2, runTick: 1,
        wallNs: 1_001_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1003, selected: observation, alternatives: [observation],
        summary: points[1], trends: points, contextSeconds: 900)

    let memory = RecordedMemoryData(selection: selection, theme: FlightdeckTheme())
    let swap = memory.plot(lines: [memory.swap, memory.swapAllocated])
    let rates = memory.plot(lines: memory.paging)

    precondition(memory.swap.values == [3, 0, nil, nil])
    precondition(memory.swapAllocated.values == [11, 0, nil, nil])
    precondition(memory.swapAllocated.strokePattern == .dashed && memory.swapAllocated.fill == 0)
    precondition(memory.paging.map(\.name) == ["Page-ins", "Compressed", "Decompressed"])
    precondition(memory.paging.map(\.strokePattern) == [.solid, .dashed, .dotted])
    precondition(memory.paging.map(\.values) == [
        [13.25, 0, nil, nil], [29.5, 0, nil, nil], [7.125, 0, nil, nil]])
    precondition(swap?.segments(line: 1) == [[0, 1]])
    precondition(rates?.segments(line: 0) == [[0, 1]])
    precondition(swap?.samples == rates?.samples)
    precondition(memory.context.target(id: points[1].id)?.identity == observation.identity)
    precondition(rates?.selectedTime(id: memory.selectedID!) == 1001)
    precondition(memory.domain.upperBound - memory.domain.lowerBound == 900)
    print("Memory swap used/allocated and three independent per-second rates retain zero and gaps")
}

private func uses_independent_swap_and_rate_theme_roles() throws {
    // Goal: custom swap and rate colors remain independent from category and temperature colors.
    // Method: replace every new role with a distinct color before projecting an empty selection.
    let selection = Selection(second: 1000, selected: nil, alternatives: [], summary: nil,
        trends: [], contextSeconds: 900)
    var theme = FlightdeckTheme()
    theme.swapUsedSeries = .pink
    theme.swapAllocatedSeries = .cyan
    theme.memoryPageinsSeries = .yellow
    theme.memoryCompressionsSeries = .green
    theme.memoryDecompressionsSeries = .mint
    theme.memoryActiveSeries = .black
    theme.memoryWiredSeries = .black
    theme.memoryCompressedSeries = .black
    theme.temperatureSeries = .black

    let memory = RecordedMemoryData(selection: selection, theme: theme)

    precondition([memory.swap.color, memory.swapAllocated.color] == [.pink, .cyan])
    precondition(memory.paging.map(\.color) == [.yellow, .green, .mint])
    print("Memory swap and rate projections use independently replaceable semantic theme roles")
}

func runRecordedMemoryThemeChecks() throws {
    // Independent replacements catch accidental use of the equal default CPU and power colors.
    let data = Data(#"""
        {"value":[{"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
          "segment":1,"memory_active_bytes":1,"memory_wired_bytes":2,
          "memory_compressed_bytes":3}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let selection = Selection(second: 1000, selected: nil, alternatives: [], summary: nil,
        trends: points, contextSeconds: 900)
    var theme = FlightdeckTheme()
    theme.memoryActiveSeries = .pink
    theme.memoryWiredSeries = .cyan
    theme.memoryCompressedSeries = .yellow

    let memory = RecordedMemoryData(selection: selection, theme: theme)

    precondition(memory.categories.map(\.color) == [theme.memoryActiveSeries,
        theme.memoryWiredSeries, theme.memoryCompressedSeries])
    print("Memory projection follows distinct replacement memory theme tokens")
}

func runRecordedMemoryBoundaryChecks() throws {
    // The adapter preserves an ordinary pair and splits changed recording identities.
    let data = Data(#"""
        {"value":[
          {"run":"first","boot":"first","publication":1,"wall_ns":1000000000000,
           "segment":1,"memory_active_bytes":1},
          {"run":"first","boot":"first","publication":2,"wall_ns":1001000000000,
           "segment":1,"memory_active_bytes":2},
          {"run":"second","boot":"first","publication":1,"wall_ns":1002000000000,
           "segment":1,"memory_active_bytes":3},
          {"run":"second","boot":"second","publication":2,"wall_ns":1003000000000,
           "segment":1,"memory_active_bytes":4},
          {"run":"second","boot":"second","publication":3,"wall_ns":1004000000000,
           "segment":2,"memory_active_bytes":5}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let observation = Observation(run: "second", boot: "second", publication: 3, runTick: 2,
        wallNs: 1_004_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1004, selected: observation, alternatives: [observation],
        summary: points[4], trends: points, contextSeconds: 900)
    let memory = RecordedMemoryData(selection: selection, theme: FlightdeckTheme())

    let plot = memory.plot(lines: memory.categories)

    precondition(plot?.segments(line: 0) == [[0, 1], [2], [3], [4]],
        "Memory adapter joined a changed run, boot or segment")
    precondition(memory.selectedID == points[4].id)
    precondition(plot?.selectedTime(id: points[4].id) == 1004)
    print("Memory adapter preserves the connected pair and splits run, boot and segment changes")
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

private func preserves_linear_plot_geometry() {
    // Overview joins observations directly, without curving their recorded trajectory.
    let points = [CGPoint(x: 0, y: 0), CGPoint(x: 1, y: 2), CGPoint(x: 2, y: 0)]
    var expected = Path()
    expected.move(to: points[0])
    expected.addLine(to: points[1])
    expected.addLine(to: points[2])
    var actual = Path()

    TimeSeriesInterpolation.linear.append(points, to: &actual, moving: true)

    precondition(actual == expected)
}

private func maps_inspection_to_the_shared_plot_frame() {
    // Cursor placement and input use the same margins and fractional wall times.
    let coordinates = TimeSeriesCoordinates(domain: 100...1000, size: CGSize(width: 615, height: 254))

    let middle = coordinates.position(at: 550.25)!
    let time = coordinates.time(at: CGPoint(x: middle, y: 100))

    precondition(abs(time! - 550.25) < 0.000001)
    precondition(coordinates.time(at: CGPoint(x: 49, y: 100)) == nil)
    precondition(coordinates.time(at: CGPoint(x: 100, y: 23)) == nil)
    precondition(coordinates.position(at: 99) == nil)
    precondition(coordinates.position(at: .infinity) == nil)
    precondition(coordinates.position(at: 100) == 50)
    precondition(coordinates.position(at: 1000) == 550)
}

private func preserves_default_curved_plot_geometry() {
    // Existing Canvas users retain their monotone curve when no interpolation is specified.
    let points = [CGPoint(x: 0, y: 0), CGPoint(x: 3, y: 6), CGPoint(x: 6, y: 0)]
    let style = TimeSeriesStyle(unit: "%")
    var expected = Path()
    expected.move(to: points[0])
    expected.addCurve(to: points[1], control1: CGPoint(x: 1, y: 2),
                      control2: CGPoint(x: 2, y: 6))
    expected.addCurve(to: points[2], control1: CGPoint(x: 4, y: 6),
                      control2: CGPoint(x: 5, y: 2))
    var actual = Path()

    style.interpolation.append(points, to: &actual, moving: true)

    precondition(actual == expected)
}

@MainActor
private func reports_the_inspected_reading_to_accessibility() throws {
    // The accessible value follows inspection, including an inspected empty second.
    let data = Data(#"""
        {"value":[
          {"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"cpu_percent":10},
          {"run":"run","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"cpu_percent":80}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let selection = Selection(second: 1001, selected: nil, alternatives: [],
        summary: points[1], trends: points, contextSeconds: 900)
    let inspection = ChartInspectionState()
    let readout: (Metric) -> String = { metricText($0.cpuPercent, unit: "%") }

    precondition(inspection.reading(in: selection, readout: readout) == readout(points[1]))
    precondition(inspection.boundary(first: true, in: selection))

    precondition(inspection.reading(in: selection, readout: readout).hasSuffix(readout(points[0])))
    inspection.hover(at: Date(timeIntervalSince1970: 999))
    precondition(inspection.reading(in: selection, readout: readout)
        .hasSuffix("No recorded observation"))
    inspection.clear()
    precondition(inspection.reading(in: selection, readout: readout) == readout(points[1]))
}

@MainActor
private func paints_soc_temperature_in_the_actual_energy_pane() throws {
    // Goal: Energy paints the acquired SoC sensor with the temperature role.
    // Method: replace only that role while rendering the actual Energy destination.
    let selection = try scalarPanelSelection(gpuPercent: 37.5)
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = selection
    let ordinary = FlightdeckTheme()
    var changed = ordinary
    changed.temperatureSeries = .white

    let baseline = scalarPanelImage(RecordedComponentPane(component: .energy),
        model: model, theme: ordinary, height: 700)
    let recolored = scalarPanelImage(RecordedComponentPane(component: .energy),
        model: model, theme: changed, height: 700)

    precondition(scalarPanelPixelDifferences(baseline, recolored) > 300,
        "The actual Energy pane did not paint its SoC temperature history")
}

@MainActor
private func scalarPanelImage<Content: View>(_ content: Content, model: RecordingModel,
                                            theme: FlightdeckTheme,
                                            height: CGFloat) -> NSBitmapImageRep {
    let renderer = ImageRenderer(content: content.environmentObject(model)
        .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
        .frame(width: 1080, height: height, alignment: .top)
        .background(theme.background).preferredColorScheme(.dark))
    renderer.scale = 1
    guard let image = renderer.cgImage else { preconditionFailure("The scalar pane did not render") }

    return NSBitmapImageRep(cgImage: image)
}

private func scalarPanelPixelDifferences(_ first: NSBitmapImageRep,
                                         _ second: NSBitmapImageRep,
                                         rows: Range<Int>? = nil,
                                         columns: Range<Int> = 0..<1080) -> Int {
    precondition(first.pixelsWide == 1080)
    precondition(second.pixelsWide == first.pixelsWide)
    precondition(second.pixelsHigh == first.pixelsHigh)
    precondition((1...2200).contains(first.pixelsHigh))
    var differences = 0
    let comparedRows = rows ?? 0..<first.pixelsHigh
    precondition(comparedRows.lowerBound >= 0)
    precondition(comparedRows.upperBound <= first.pixelsHigh)
    precondition(columns.lowerBound >= 0)
    precondition(columns.upperBound <= first.pixelsWide)
    for y in comparedRows {
        for x in columns {
            guard let source = first.colorAt(x: x, y: y)?.usingColorSpace(.sRGB),
                  let target = second.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else {
                preconditionFailure("The rendered scalar panel has no RGB pixel")
            }

            let distance = abs(source.redComponent - target.redComponent)
                + abs(source.greenComponent - target.greenComponent)
                + abs(source.blueComponent - target.blueComponent)
            if distance > 0.06 { differences += 1 }
        }
    }

    return differences
}

private func scalarPanelSelection(gpuPercent: Double) throws -> Selection {
    let values: [[String: Any]] = [
        ["run": "scalar", "boot": "boot", "publication": 1,
         "wall_ns": 1_000_000_000_000, "segment": 1, "gpu_active_percent": gpuPercent,
         "gpu_watts": 2, "power_watts": 12, "soc_celsius": 45],
        ["run": "scalar", "boot": "boot", "publication": 2,
         "wall_ns": 1_899_000_000_000, "segment": 1, "gpu_active_percent": gpuPercent,
         "gpu_watts": 2, "power_watts": 12, "soc_celsius": 65]]
    let points: [Metric] = try decodeRecordingResponse(
        JSONSerialization.data(withJSONObject: ["value": values]))
    let selected = Observation(run: "scalar", boot: "boot", publication: 2, runTick: 2,
        wallNs: 1_899_000_000_000, windowLosses: nil)

    return Selection(second: 1899, selected: selected, alternatives: [selected],
        summary: points[1], trends: points, contextSeconds: 900)
}

@MainActor
private func paints_gpu_activity_in_the_actual_overview() throws {
    // Goal: Overview paints GPU activity with its independent semantic role.
    // Method: recolor only activity while rendering the actual Overview destination.
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = try scalarPanelSelection(gpuPercent: 37.5)
    let ordinary = FlightdeckTheme()
    var changed = ordinary
    changed.gpuActivitySeries = .white

    let baseline = scalarPanelImage(Dashboard(), model: model, theme: ordinary, height: 1800)
    let recolored = scalarPanelImage(Dashboard(), model: model, theme: changed, height: 1800)

    precondition(scalarPanelPixelDifferences(baseline, recolored) > 300,
        "The actual Overview did not paint GPU activity with its semantic role")
}

@MainActor
private func reads_gpu_activity_in_the_actual_overview() throws {
    // Goal: Overview reads activity rather than the independent GPU power field.
    // Method: vary only activity and compare the grid above the schematic.
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = try scalarPanelSelection(gpuPercent: 10)
    let low = scalarPanelImage(Dashboard(), model: model, theme: FlightdeckTheme(), height: 1800)
    model.selection = try scalarPanelSelection(gpuPercent: 80)

    let high = scalarPanelImage(Dashboard(), model: model, theme: FlightdeckTheme(), height: 1800)

    precondition(scalarPanelPixelDifferences(low, high, rows: 0..<700) > 500,
        "The actual Overview ignored changed GPU activity while GPU power stayed constant")
}

@MainActor
private func paints_gpu_activity_in_the_actual_gpu_pane() throws {
    // Goal: GPU activity uses its own role while GPU power keeps its independent color.
    // Method: replace only activity while rendering the actual GPU destination.
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = try scalarPanelSelection(gpuPercent: 37.5)
    let ordinary = FlightdeckTheme()
    var changed = ordinary
    changed.gpuActivitySeries = .white

    let baseline = scalarPanelImage(RecordedComponentPane(component: .gpu),
        model: model, theme: ordinary, height: 700)
    let recolored = scalarPanelImage(RecordedComponentPane(component: .gpu),
        model: model, theme: changed, height: 700)

    precondition(scalarPanelPixelDifferences(baseline, recolored) > 300,
        "The actual GPU pane did not paint activity with its semantic role")
}

private func preserves_soc_temperature_values_units_and_exact_selection() throws {
    // Goal: the single acquired sensor retains Celsius, zero and unavailable readings.
    // Method: decode literal sensor readings and a repeated-second alternative.
    let data = Data(#"""
        {"value":[
          {"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"soc_celsius":48.25,"power_watts":12},
          {"run":"run","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"soc_celsius":0,"power_watts":13},
          {"run":"run","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":1,"soc_celsius":null},
          {"run":"run","boot":"boot","publication":4,"wall_ns":1003000000000,
           "segment":1},
          {"run":"other","boot":"other","publication":1,"wall_ns":1003000000000,
           "segment":2,"soc_celsius":71.5}]}
        """#.utf8)
    let points: [Metric] = try decodeRecordingResponse(data)
    let selected = Observation(run: "run", boot: "boot", publication: 4, runTick: 4,
        wallNs: 1_003_000_000_000, windowLosses: nil)
    let selection = Selection(second: 1003, selected: selected, alternatives: [selected],
        summary: points[3], trends: points, contextSeconds: 900)

    let component = RecordedComponentData(context: RecordedChartContext(selection: selection),
        theme: FlightdeckTheme())
    let temperature = component.context.plot(lines: [component.socTemperature])
    let power = component.context.plot(lines: component.energy)

    precondition(component.socTemperature.values == [48.25, 0, nil, nil, 71.5])
    precondition(component.socTemperature.name == "SoC temperature")
    precondition(component.socTemperature.unit == "°C")
    precondition(component.socTemperature.negative != true)
    precondition(dashboardReading(component.socTemperature.values[1], unit: "°C") == "0 °C")
    precondition(dashboardReading(component.socTemperature.values[2], unit: "°C") == "Unavailable")
    precondition(temperature?.samples == power?.samples)
    precondition(temperature?.segments(line: 0) == [[0, 1], [4]])
    precondition(component.context.domain == 104...1004)
    precondition(component.context.selectedID == points[3].id)
    precondition(component.context.target(id: points[4].id) == RecordedSelectionTarget(second: 1003,
        identity: points[4].identity))
    precondition(component.energy.map(\.values) == [[12, 13, nil, nil, nil],
        [nil, nil, nil, nil, nil]])
}

private func uses_independent_gpu_activity_and_temperature_roles() throws {
    // Goal: activity, power and temperature remain independently replaceable.
    // Method: give each semantic role a distinct color before projecting real scalar data.
    let selection = try scalarPanelSelection(gpuPercent: 37.5)
    var theme = FlightdeckTheme()
    theme.gpuActivitySeries = .pink
    theme.gpuPowerSeries = .cyan
    theme.temperatureSeries = .yellow

    let data = RecordedComponentData(context: RecordedChartContext(selection: selection), theme: theme)

    precondition(data.gpuActivity.color == .pink)
    precondition(data.gpuPower.color == .cyan)
    precondition(data.socTemperature.color == .yellow)
    precondition(data.gpuActivity.unit == "%")
    precondition(data.gpuPower.unit == "W")
}

@MainActor
private func separates_gpu_power_in_the_actual_energy_pane() throws {
    // Goal: Energy keeps the GPU-power chart separate from the system-power chart.
    // Method: recolor GPU power and inspect the chart row below the summary cards.
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = try scalarPanelSelection(gpuPercent: 37.5)
    let ordinary = FlightdeckTheme()
    var changed = ordinary
    changed.gpuPowerSeries = .white

    let baseline = scalarPanelImage(RecordedComponentPane(component: .energy),
        model: model, theme: ordinary, height: 700)
    let recolored = scalarPanelImage(RecordedComponentPane(component: .energy),
        model: model, theme: changed, height: 700)

    precondition(scalarPanelPixelDifferences(baseline, recolored,
        rows: 160..<400, columns: 0..<500) == 0,
        "Energy painted GPU power inside the System power panel")
    precondition(scalarPanelPixelDifferences(baseline, recolored,
        rows: 160..<400, columns: 580..<1080) > 300,
        "Energy omitted GPU power from its dedicated panel")
}

@MainActor
private func separates_system_power_in_the_actual_energy_pane() throws {
    // Goal: Energy keeps the system-power chart separate from the GPU-power chart.
    // Method: recolor system power and inspect the chart row below the summary cards.
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = try scalarPanelSelection(gpuPercent: 37.5)
    let ordinary = FlightdeckTheme()
    var changed = ordinary
    changed.systemPowerSeries = .white

    let baseline = scalarPanelImage(RecordedComponentPane(component: .energy),
        model: model, theme: ordinary, height: 700)
    let recolored = scalarPanelImage(RecordedComponentPane(component: .energy),
        model: model, theme: changed, height: 700)

    precondition(scalarPanelPixelDifferences(baseline, recolored,
        rows: 160..<400, columns: 580..<1080) == 0,
        "Energy painted system power inside the GPU power panel")
    precondition(scalarPanelPixelDifferences(baseline, recolored,
        rows: 160..<400, columns: 0..<500) > 300,
        "Energy omitted system power from its dedicated panel")
}

@MainActor
private func expands_the_actual_overview_for_gpu_activity_above_nominal_capacity() throws {
    // Goal: the actual Overview keeps measured activity above 100% inside its plot.
    // Method: recolor a constant 150% line below the summary row; its swatch cannot pass alone.
    let model = RecordingModel(path: "/unused", pollingEnabled: false)
    model.selection = try scalarPanelSelection(gpuPercent: 150)
    let ordinary = FlightdeckTheme()
    var changed = ordinary
    changed.gpuActivitySeries = .white

    let baseline = scalarPanelImage(Dashboard(), model: model, theme: ordinary, height: 1800)
    let recolored = scalarPanelImage(Dashboard(), model: model, theme: changed, height: 1800)
    let plotPixels = scalarPanelPixelDifferences(baseline, recolored,
        rows: 150..<1800, columns: 0..<1080)

    precondition(plotPixels > 300,
        "Overview clipped measured 150% GPU activity outside its plot: \(plotPixels) role pixels")
    print("Overview 150% GPU activity paints \(plotPixels) role pixels inside its plot rectangle")
}
