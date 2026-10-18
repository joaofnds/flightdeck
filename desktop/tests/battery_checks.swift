import AppKit
import Foundation
import SwiftUI

@MainActor
func runBatteryChecks() throws {
    try decodes_battery_zero_signed_flow_and_health_above_design()
    try keeps_legacy_and_null_battery_unavailable()
    try projects_actual_battery_charts_with_percent_and_signed_watts()
    try preserves_exact_battery_identity_without_filling_unobserved_seconds()
    try refuses_battery_hover_and_selection_while_busy()
    try selects_the_exact_inspected_battery_alternative()
    try maps_selected_battery_readings_to_the_actual_schematic()
    try paints_the_actual_battery_chart_roles_and_zero_state()
}

private func decodes_battery_zero_signed_flow_and_health_above_design() throws {
    // Goal: exact controller zero, signed discharge and health above design survive decoding.
    // Method: decode the actual snake-case response envelope into Metric.
    let metric: Metric = try decodeRecordingResponse(Data(#"""
        {"value":{"run":"run","boot":"boot","publication":9007199254740993,
          "wall_ns":1000000000000,"segment":1,"battery":{
            "source_continuous_ns":123,"age_seconds":0,"charge_percent":0,
            "health_percent":101.4,"cycle_count":0,"charging":false,
            "external_power":true,"power_watts":-12.5,"adapter_watts":94}}}
        """#.utf8))

    precondition(metric.battery?.chargePercent == 0)
    precondition(metric.battery?.healthPercent == 101.4)
    precondition(metric.battery?.cycleCount == 0)
    precondition(metric.battery?.charging == false)
    precondition(metric.battery?.externalPower == true)
    precondition(metric.battery?.powerWatts == -12.5)
    precondition(metric.battery?.adapterWatts == 94)
    precondition(metric.battery?.sourceContinuousNs == 123)
}

private func keeps_legacy_and_null_battery_unavailable() throws {
    // Goal: legacy recordings and unavailable optional fields cannot become captured values.
    // Method: decode legacy, null battery and independently null battery fields.
    let points: [Metric] = try decodeRecordingResponse(Data(#"""
        {"value":[
          {"run":"r","boot":"b","publication":1,"wall_ns":1000000000000,"segment":1},
          {"run":"r","boot":"b","publication":2,"wall_ns":1001000000000,"segment":1,
           "battery":null},
          {"run":"r","boot":"b","publication":3,"wall_ns":1002000000000,"segment":1,
           "battery":{"source_continuous_ns":0,"age_seconds":60,"charge_percent":null,
             "health_percent":102,"cycle_count":null,"charging":null,"external_power":null,
             "power_watts":0,"adapter_watts":null}}]}
        """#.utf8))

    precondition(points[0].battery == nil)
    precondition(points[1].battery == nil)
    precondition(points[2].battery?.chargePercent == nil)
    precondition(points[2].battery?.charging == nil)
    precondition(points[2].battery?.cycleCount == nil)
    precondition(points[2].battery?.healthPercent == 102)
    precondition(points[2].battery?.powerWatts == 0)
    precondition(points[2].battery?.ageSeconds == 60)
}

@MainActor
private func projects_actual_battery_charts_with_percent_and_signed_watts() throws {
    // Goal: the actual pane's chart configuration preserves numeric units, magnitude and gaps.
    // Method: project literal readings and inspect the chart values used by the pane.
    let points = try batteryPoints()
    let data = RecordedBatteryData(context: RecordedChartContext(selection: batterySelection(points)),
        theme: FlightdeckTheme())

    let charge = RecordedBattery.chargeChart(data, inspectedID: .constant(nil),
        inspecting: .constant(false))
    let flow = RecordedBattery.flowChart(data, inspectedID: .constant(nil),
        inspecting: .constant(false))

    precondition(charge.title == "Battery charge")
    precondition(charge.style.unit == "%")
    precondition(charge.style.minimum == 0)
    precondition(charge.style.maximum == 100)
    precondition(charge.lines[0].values == [55, 0, nil, 55])
    precondition(flow.title == "Battery power flow")
    precondition(flow.style.unit == "W")
    precondition(flow.lines[0].values == [-12.5, 0, nil, 9])
    precondition(flow.lines[0].negative != true)
    precondition(data.context.plot(lines: [data.flow])?.segments(line: 0) == [[0, 1], [3]])
    precondition(data.details.first { $0.0 == "Health" }?.1.contains("101") == true)
    precondition(data.details.first { $0.0 == "Cycles" }?.1 == "11")
}

private func preserves_exact_battery_identity_without_filling_unobserved_seconds() throws {
    // Goal: binary state paints exact observed seconds while zeros and gaps remain distinct.
    // Method: decode adjacent large-publication alternatives and inspect the production timeline.
    let points = try batteryPoints()
    let selection = batterySelection(points, selected: points[0])

    let timeline = BatteryChargingTimeline(selection: selection)

    precondition(timeline?.cells.count == 900)
    precondition(timeline?.firstSecond == 1000)
    precondition(timeline?.endSecond == 1900)
    precondition(timeline?.state(id: points[0].id) == .notCharging)
    precondition(timeline?.state(id: points[1].id) == .charging)
    precondition(timeline?.state(id: points[2].id) == nil)
    precondition(timeline?.point(at: 0.1 / 900)?.id == points[0].id)
    precondition(timeline?.point(at: 5.5 / 900) == nil)
    precondition(timeline?.target(id: points[3].id)?.identity == points[3].identity)
    precondition(timeline?.step(id: points[2].id, offset: 1)?.id == points[3].id)
    precondition(timeline?.point(at: .nan) == nil)
    precondition(timeline?.point(at: 1) == nil)
    precondition(BatteryChargingTimeline(selection: batterySelection([points[0], points[0]])) == nil)
}

@MainActor
private func refuses_battery_hover_and_selection_while_busy() throws {
    // Goal: an in-flight selection cannot be changed by pointer or keyboard interaction.
    // Method: invoke the same input adapter used by the binary timeline while busy.
    let fixture = try BatteryInputFixture()
    let input = fixture.input(busy: true)

    input.inspect(at: CGPoint(x: 50.5, y: 83), size: CGSize(width: 1015, height: 190))
    precondition(input.step(offset: 1) == .ignored)
    precondition(input.boundary(first: false) == .ignored)
    precondition(input.select() == .ignored)
    precondition(input.clear() == .ignored)
    precondition(fixture.inspectedID == nil)
    precondition(!fixture.inspecting)
    precondition(fixture.targets.isEmpty)
}

@MainActor
private func selects_the_exact_inspected_battery_alternative() throws {
    // Goal: shared inspection stays local until Return selects the exact alternate identity.
    // Method: step and confirm through the production binary-timeline interaction adapter.
    let fixture = try BatteryInputFixture()
    let input = fixture.input(busy: false)

    precondition(input.step(offset: 1) == .handled)
    precondition(fixture.inspectedID == fixture.points[1].id)
    precondition(fixture.targets.isEmpty)
    precondition(input.select() == .handled)

    precondition(fixture.targets == [RecordedSelectionTarget(second: 1000,
        identity: fixture.points[1].identity)])
    precondition(fixture.inspectedID == nil)
    precondition(!fixture.inspecting)
}

private func maps_selected_battery_readings_to_the_actual_schematic() throws {
    // Goal: the real schematic borrows only the matching observation's battery values.
    // Method: map a decoded source reading, then repeat with a mismatched summary identity.
    let points = try batteryPoints()
    let matching = batterySelection(points)
    let wrong = Selection(second: matching.second, selected: matching.selected,
        alternatives: [], summary: points[0], trends: points, contextSeconds: 900)

    let readings = schematicReadings(selection: matching, fans: nil)
    let unavailable = schematicReadings(selection: wrong, fans: nil)

    precondition(readings.batteryPercent == 55)
    precondition(readings.batteryWatts == 9)
    precondition(unavailable.batteryPercent == nil)
    precondition(unavailable.batteryWatts == nil)
    precondition(RecordedBatteryData(context: RecordedChartContext(selection: wrong),
        theme: FlightdeckTheme()).selected == nil)
}

@MainActor
private func paints_the_actual_battery_chart_roles_and_zero_state() throws {
    // Goal: the actual pane uses semantic charge/flow roles and paints Not charging distinctly.
    // Method: render the real pane with isolated green and magenta theme roles.
    let points = try (0..<900).map { index in
        try batteryPoint(publication: UInt64(index + 1), second: UInt64(1000 + index),
            charge: 55, watts: -12.5, charging: false)
    }
    let model = RecordingModel(path: "", pollingEnabled: false)
    model.selection = batterySelection(points)
    var theme = FlightdeckTheme()
    theme.batteryChargeSeries = Color(red: 0, green: 1, blue: 0)
    theme.batteryFlowSeries = Color(red: 1, green: 0, blue: 1)
    theme.batteryNotCharging = Color(red: 0, green: 1, blue: 1)
    let renderer = ImageRenderer(content: RecordedBattery().environmentObject(model)
        .frame(width: 1080, height: 1100, alignment: .top)
        .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
        .background(theme.background).preferredColorScheme(.dark))
    renderer.scale = 1
    guard let image = renderer.cgImage else { preconditionFailure("Battery did not render") }
    let bitmap = NSBitmapImageRep(cgImage: image)

    let charge = batteryPixels(bitmap, role: 0)
    let flow = batteryPixels(bitmap, role: 1)
    let notCharging = batteryPixels(bitmap, role: 2)

    precondition(charge > 300, "The actual pane did not paint the battery charge role")
    precondition(flow > 300, "The actual pane did not paint the battery flow role")
    precondition(notCharging > 1000, "Not charging did not paint a binary state rail")
    print("Battery native decoding, charts, semantic roles, zero-state timeline and exact input checks passed")
}

private func batteryPixels(_ image: NSBitmapImageRep, role: Int) -> Int {
    var count = 0
    for y in 150..<min(850, image.pixelsHigh) {
        for x in 85..<min(980, image.pixelsWide) {
            guard let color = image.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) else { continue }

            let match: Bool
            switch role {
            case 0: match = color.greenComponent > 0.7 && color.redComponent < 0.4
                && color.blueComponent < 0.4
            case 1: match = color.redComponent > 0.7 && color.blueComponent > 0.3
                && color.greenComponent < 0.5
            default: match = color.greenComponent > 0.7 && color.blueComponent > 0.7
                && color.redComponent < 0.4
            }
            if match { count += 1 }
        }
    }
    return count
}

@MainActor
private final class BatteryInputFixture {
    let points: [Metric]
    let timeline: BatteryChargingTimeline
    var inspectedID: String?
    var inspecting = false
    var targets: [RecordedSelectionTarget] = []

    init() throws {
        points = try batteryPoints()
        timeline = BatteryChargingTimeline(selection: batterySelection(points, selected: points[0]))!
    }

    func input(busy: Bool) -> BatteryChargingInteraction {
        BatteryChargingInteraction(timeline: timeline,
            inspectedID: Binding(get: { self.inspectedID }, set: { self.inspectedID = $0 }),
            inspecting: Binding(get: { self.inspecting }, set: { self.inspecting = $0 }),
            busy: busy, onSelect: { self.targets.append($0) })
    }
}

private func batteryPoints() throws -> [Metric] {
    try [batteryPoint(publication: 9_007_199_254_740_993, second: 1000,
            charge: 55, watts: -12.5, charging: false),
         batteryPoint(publication: 9_007_199_254_740_994, second: 1000,
            charge: 0, watts: 0, charging: true, nanoseconds: 900_000_000),
         batteryPoint(publication: 3, second: 1002, charge: nil, watts: nil, charging: nil),
         batteryPoint(publication: 4, second: 1899, charge: 55, watts: 9, charging: true)]
}

private func batteryPoint(publication: UInt64, second: UInt64,
                          charge: Double?, watts: Double?, charging: Bool?,
                          nanoseconds: UInt64 = 0) throws -> Metric {
    precondition(nanoseconds < 1_000_000_000)
    var battery: [String: Any] = ["source_continuous_ns": 123, "age_seconds": 0,
        "health_percent": 101.4, "cycle_count": 11]
    battery["charge_percent"] = charge
    battery["power_watts"] = watts
    battery["charging"] = charging
    let value: [String: Any] = ["run": "run", "boot": "boot", "publication": publication,
        "wall_ns": second * 1_000_000_000 + nanoseconds, "segment": 1, "battery": battery]

    return try decodeRecordingResponse(JSONSerialization.data(withJSONObject: ["value": value]))
}

private func batterySelection(_ points: [Metric], selected: Metric? = nil) -> Selection {
    let chosen = selected ?? points.last!
    let observation = Observation(run: chosen.run, boot: chosen.boot,
        publication: chosen.publication, runTick: chosen.publication, wallNs: chosen.wallNs,
        windowLosses: nil)

    return Selection(second: 1899, selected: observation, alternatives: [observation],
        summary: chosen, trends: points, contextSeconds: 900)
}
