import AppKit
import Foundation
import SwiftUI

@MainActor
func runStorageChecks() throws {
    try decodes_physical_disk_zeros_and_independent_missing_fields()
    try keeps_legacy_storage_and_a_failed_scan_distinct_from_an_empty_scan()
    try projects_storage_units_mirrored_writes_and_gaps()
    try uses_the_exact_selected_storage_observation()
    try paints_the_actual_storage_pane_with_semantic_roles()
}

private func keeps_legacy_storage_and_a_failed_scan_distinct_from_an_empty_scan() throws {
    // Goal: unavailable legacy data and failed acquisition cannot look like an idle device.
    // Method: decode missing, failed and complete-empty source states through the bridge envelope.
    let points: [Metric] = try decodeRecordingResponse(Data(#"""
        {"value":[
          {"run":"r","boot":"b","publication":1,"wall_ns":1000000000000,"segment":1},
          {"run":"r","boot":"b","publication":2,"wall_ns":1001000000000,"segment":1,
           "storage":{"state":"failed","devices":[]}},
          {"run":"r","boot":"b","publication":3,"wall_ns":1002000000000,"segment":1,
           "storage":{"state":"complete","devices":[],"read_bps":0,"write_bps":0}}]}
        """#.utf8))

    precondition(points[0].storage == nil)
    precondition(points[1].storage?.state == .failed)
    precondition(points[1].storage?.rates.readBps == nil)
    precondition(points[1].storage?.state.notice != nil)
    precondition(points[2].storage?.state == .complete)
    precondition(points[2].storage?.rates.readBps == 0)
    precondition(points[2].storage?.devices.isEmpty == true)
    precondition(points[2].storage?.state.notice == nil)
}

@MainActor
private func projects_storage_units_mirrored_writes_and_gaps() throws {
    // Goal: machine charts preserve decimal throughput, completed operations and unavailable intervals.
    // Method: inspect the actual pane's lines and style with measured zero and a source gap.
    let points = try [storagePoint(publication: 1, second: 1000, read: 0, write: 1_000_000),
        storagePoint(publication: 2, second: 1001, read: nil, write: nil, captured: false),
        storagePoint(publication: 3, second: 1899, read: 3_000_000, write: 6_000_000)]
    let data = RecordedStorageData(context: RecordedChartContext(selection: storageSelection(points)),
        theme: FlightdeckTheme())
    let throughput = RecordedStorage.throughputChart(data, inspectedID: .constant(nil),
        inspecting: .constant(false))
    let operations = RecordedStorage.operationsChart(data, inspectedID: .constant(nil),
        inspecting: .constant(false))

    precondition(throughput.style.unit == "MB/s")
    precondition(throughput.lines[0].values == [0, nil, 3])
    precondition(throughput.lines[1].values == [1, nil, 6])
    precondition(throughput.lines[1].negative == true)
    precondition(operations.style.unit == "ops/s")
    precondition(operations.lines[0].values == [120, nil, 120])
    precondition(operations.lines[1].negative == true)
    precondition(data.meanTime[0].values == [0.25, nil, 0.25])
    precondition(data.errors[0].values == [0, nil, 0])
    precondition(data.context.plot(lines: data.throughput)?.segments(line: 0) == [[0], [2]])
    precondition(dashboardReading(0.0002, unit: "MB/s").contains("B/s"))
}

private func uses_the_exact_selected_storage_observation() throws {
    // Goal: same-second alternatives cannot lend another observation's disk summary or device list.
    // Method: match and mismatch run/publication identity while retaining one wall second.
    let first = try storagePoint(publication: 9_007_199_254_740_993, second: 1899,
        read: 1_000_000, write: 2_000_000)
    let second = try storagePoint(publication: 9_007_199_254_740_994, second: 1899,
        read: 3_000_000, write: 4_000_000)
    let selected = Observation(run: second.run, boot: second.boot, publication: second.publication,
        runTick: second.publication, wallNs: second.wallNs, windowLosses: nil)
    let matching = storageSelection([first, second])
    let wrong = Selection(second: 1899, selected: selected, alternatives: [selected], summary: first,
        trends: [first, second], contextSeconds: 900)
    let goodData = RecordedStorageData(context: RecordedChartContext(selection: matching),
        theme: FlightdeckTheme())
    let wrongData = RecordedStorageData(context: RecordedChartContext(selection: wrong),
        theme: FlightdeckTheme())

    precondition(goodData.selected?.rates.readBps == 3_000_000)
    precondition(wrongData.selected == nil)
    precondition(goodData.context.point(at: 1899.5)?.id == second.id)
    precondition(goodData.context.target(id: first.id)?.identity == first.identity)
    precondition(goodData.context.target(id: second.id)?.identity == second.identity)
}

@MainActor
private func paints_the_actual_storage_pane_with_semantic_roles() throws {
    // Goal: the actual expanded pane paints its throughput and operation roles using theme colors.
    // Method: render 900 observations with isolated green and magenta read/write roles.
    let points: [Metric] = try (0..<900).map { index in
        let read: Double = 2_000_000 + Double(index % 90) * 10_000
        let write: Double = 1_000_000 + Double(index % 60) * 10_000

        return try storagePoint(publication: UInt64(index + 1), second: UInt64(1000 + index),
            read: read, write: write)
    }
    let model = RecordingModel(path: "", pollingEnabled: false)
    model.selection = storageSelection(points)
    var theme = FlightdeckTheme()
    theme.storageReadSeries = Color(red: 0, green: 1, blue: 0)
    theme.storageWriteSeries = Color(red: 1, green: 0, blue: 1)
    let renderer = ImageRenderer(content: RecordedStorage().environmentObject(model)
        .frame(width: 1080, height: 1100, alignment: .top)
        .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
        .background(theme.background).preferredColorScheme(.dark))
    renderer.scale = 1
    guard let image = renderer.cgImage else { preconditionFailure("Storage did not render") }
    let bitmap = NSBitmapImageRep(cgImage: image)

    precondition(storageRolePixels(bitmap, write: false) > 500,
        "The actual Storage pane did not paint the read role")
    precondition(storageRolePixels(bitmap, write: true) > 500,
        "The actual Storage pane did not paint the write role")
    print("Storage native exact decoding, unavailable states, chart units, gaps and semantic role checks passed")
}

private func storageRolePixels(_ image: NSBitmapImageRep, write: Bool) -> Int {
    var count = 0
    for y in 150..<min(850, image.pixelsHigh) {
        for x in 85..<min(980, image.pixelsWide) {
            guard let color = image.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) else { continue }

            let matches = write
                ? color.redComponent > 0.7 && color.blueComponent > 0.7 && color.greenComponent < 0.4
                : color.greenComponent > 0.7 && color.redComponent < 0.4 && color.blueComponent < 0.4
            if matches { count += 1 }
        }
    }
    return count
}

private func storagePoint(publication: UInt64, second: UInt64, read: Double?, write: Double?,
                          captured: Bool = true) throws -> Metric {
    var rates: [String: Any] = ["read_ops_per_second": 120, "write_ops_per_second": 80,
        "read_mean_ms": 0.25, "write_mean_ms": 0.5,
        "read_errors_per_second": 0, "write_errors_per_second": 0]
    rates["read_bps"] = read
    rates["write_bps"] = write
    var device = rates
    device["driver_id"] = 123
    device["media_id"] = 456
    device["name"] = "disk0"
    var storage = rates
    storage["state"] = "complete"
    storage["devices"] = [device]
    var value: [String: Any] = ["run": "run", "boot": "boot", "publication": publication,
        "wall_ns": second * 1_000_000_000, "segment": 1]
    if captured { value["storage"] = storage }

    return try decodeRecordingResponse(JSONSerialization.data(withJSONObject: ["value": value]))
}

private func storageSelection(_ points: [Metric]) -> Selection {
    let chosen = points.last!
    let observation = Observation(run: chosen.run, boot: chosen.boot,
        publication: chosen.publication, runTick: chosen.publication, wallNs: chosen.wallNs,
        windowLosses: nil)

    return Selection(second: 1899, selected: observation, alternatives: [observation],
        summary: chosen, trends: points, contextSeconds: 900)
}

private func decodes_physical_disk_zeros_and_independent_missing_fields() throws {
    // Goal: storage zero and a missing direction stay distinct with exact device identities.
    // Method: decode a real response shape through the native boundary.
    let metric: Metric = try decodeRecordingResponse(Data(#"""
        {"value":{"run":"r","boot":"b","publication":9007199254740993,
          "wall_ns":1000000000000,"segment":1,"storage":{"state":"complete",
            "read_bps":0,"write_bps":null,"read_ops_per_second":0,
            "write_ops_per_second":null,"read_mean_ms":null,"write_mean_ms":0.125,
            "read_errors_per_second":0,"write_errors_per_second":0,
            "devices":[{"driver_id":18446744073709551615,"media_id":9007199254740993,
              "name":"disk0","read_bps":0,"write_bps":null,
              "read_ops_per_second":0,"write_ops_per_second":null,
              "read_mean_ms":null,"write_mean_ms":0.125,
              "read_errors_per_second":0,"write_errors_per_second":0}]}}}
        """#.utf8))

    precondition(metric.storage?.rates.readBps == 0)
    precondition(metric.storage?.rates.writeBps == nil)
    precondition(metric.storage?.devices.first?.driverId == UInt64.max)
    precondition(metric.storage?.devices.first?.mediaId == 9_007_199_254_740_993)
}
