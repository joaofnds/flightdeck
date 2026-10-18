import AppKit
import Foundation
import SwiftUI

@MainActor
func runMemoryQuantitiesChecks() throws {
    try projects_all_thirteen_selected_source_quantities_with_binary_readouts()
    try preserves_zero_null_and_absent_source_quantities()
    try keeps_all_six_measured_zeros_available()
    try keeps_a_present_summary_without_quantities_unavailable()
    try leaves_all_quantities_unavailable_without_a_selected_observation()
    try leaves_quantities_unavailable_without_the_selected_summary()
    try refuses_a_different_run_summary()
    try refuses_a_different_boot_summary()
    try refuses_a_different_publication_summary()
    try keeps_unknown_pressure_unavailable()
    try quantityRenderGuard(field: "memory_free_bytes")
}

private func projects_all_thirteen_selected_source_quantities_with_binary_readouts() throws {
    // Goal: every source category keeps its name and binary quantity at the selected observation.
    // Method: decode independent literal byte counts and compare the complete bounded row projection.
    let point: Metric = try decodeRecordingResponse(Data(#"""
        {"value":{"run":"selected","boot":"boot","publication":7,
         "wall_ns":1000500000000,"segment":1,
         "memory_total_bytes":51539607552,"memory_active_bytes":1073741824,
         "memory_wired_bytes":2147483648,"memory_compressed_bytes":3221225472,
         "memory_free_bytes":4294967296,"memory_inactive_bytes":5368709120,
         "memory_speculative_bytes":268435456,"memory_anonymous_bytes":7516192768,
         "memory_file_backed_bytes":8589934592,"memory_purgeable_bytes":536870912,
         "swap_used_bytes":9663676416,"swap_total_bytes":10737418240,"pressure":1}}
        """#.utf8))
    let selection = quantitySelection(point)

    let data = MemoryQuantitiesData(selection: selection)

    precondition(data.rows.map(\.name) == ["Installed", "Active", "Wired", "Compressed",
        "Free", "Inactive", "Speculative", "Anonymous", "File-backed", "Purgeable",
        "Swap used", "Swap allocated", "Memory pressure"])
    precondition(data.rows.map(\.reading) == ["48 GiB", "1 GiB", "2 GiB", "3 GiB",
        "4 GiB", "5 GiB", "256 MiB", "7 GiB", "8 GiB", "512 MiB", "9 GiB", "10 GiB", "Warning"])
    precondition(data.identity == point.identity)
    precondition(data.date == Date(timeIntervalSince1970: 1000.5))
    precondition(point.memoryFreeBytes == 4_294_967_296)
    precondition(point.memoryInactiveBytes == 5_368_709_120)
    precondition(point.memorySpeculativeBytes == 268_435_456)
    precondition(point.memoryAnonymousBytes == 7_516_192_768)
    precondition(point.memoryFileBackedBytes == 8_589_934_592)
    precondition(point.memoryPurgeableBytes == 536_870_912)
    print("All thirteen Memory quantity rows retain literal binary values and selected time")
}

private func quantitySelection(_ point: Metric) -> Selection {
    let observation = Observation(run: point.run, boot: point.boot,
        publication: point.publication, runTick: 1, wallNs: point.wallNs, windowLosses: nil)

    return Selection(second: point.wallNs / 1_000_000_000, selected: observation,
        alternatives: [observation], summary: point, trends: [point], contextSeconds: 900)
}

private func preserves_zero_null_and_absent_source_quantities() throws {
    // Goal: measured zeros stay available independently of null and absent source categories.
    // Method: project a literal mixed source summary without reading another observation.
    let point = try quantityPoint(fields: #"""
        "memory_free_bytes":0,"memory_inactive_bytes":null,"memory_speculative_bytes":0,
        "memory_anonymous_bytes":null,"memory_file_backed_bytes":0,"pressure":0
        """#)

    let data = MemoryQuantitiesData(selection: quantitySelection(point))

    precondition(Array(data.rows[4...9]).map(\.reading)
        == ["0 GiB", "Unavailable", "0 GiB", "Unavailable", "0 GiB", "Unavailable"])
    precondition(data.rows[12].reading == "Normal")
    print("Independent zero, null and absent Memory quantities remain distinct")
}

private func leaves_all_quantities_unavailable_without_a_selected_observation() throws {
    // Goal: a gap cannot borrow a populated summary or trend from another observation.
    // Method: retain measured fields in both summary and history while selecting no observation.
    let point = try quantityPoint(fields: #"""
        "memory_free_bytes":4294967296,"memory_total_bytes":51539607552,"pressure":2
        """#)
    let selection = Selection(second: 1001, selected: nil, alternatives: [], summary: point,
        trends: [point], contextSeconds: 900)

    let data = MemoryQuantitiesData(selection: selection)

    precondition(data.rows.map(\.reading) == Array(repeating: "Unavailable", count: 13))
    precondition(data.identity == nil)
    precondition(data.date == nil)
    print("Unselected Memory quantities cannot borrow a populated summary or history")
}

private func leaves_quantities_unavailable_without_the_selected_summary() throws {
    // Goal: a selected observation without a metric summary keeps readings unavailable.
    // Method: leave a measured trend in context while withholding its selected summary.
    let point = try quantityPoint(fields: #"""
        "memory_free_bytes":4294967296,"memory_total_bytes":51539607552,"pressure":2
        """#)
    let selected = quantitySelection(point).selected!
    let selection = Selection(second: 1000, selected: selected, alternatives: [selected],
        summary: nil, trends: [point], contextSeconds: 900)

    let data = MemoryQuantitiesData(selection: selection)

    precondition(data.rows.map(\.reading) == Array(repeating: "Unavailable", count: 13))
    precondition(data.date == Date(timeIntervalSince1970: 1000.5))
    print("Missing selected summaries stay unavailable despite a measured context trend")
}

private func refuses_a_different_run_summary() throws {
    // Goal: same-second observations in another run cannot supply selected quantities.
    // Method: oppose a populated other-run summary to the exact selected observation.
    try quantityRejectsSummary(run: "other", boot: "boot", publication: 7)
}

private func refuses_a_different_boot_summary() throws {
    // Goal: the selected run's other boot cannot supply quantities.
    // Method: change only the summary boot identity while retaining its measured fields.
    try quantityRejectsSummary(run: "selected", boot: "other", publication: 7)
}

private func refuses_a_different_publication_summary() throws {
    // Goal: a same-second sibling publication cannot supply selected quantities.
    // Method: change only the summary publication while retaining its measured fields.
    try quantityRejectsSummary(run: "selected", boot: "boot", publication: 8)
}

private func quantityRejectsSummary(run: String, boot: String, publication: UInt64) throws {
    let selectedPoint = try quantityPoint(fields: "\"memory_free_bytes\":0")
    let other: Metric = try decodeRecordingResponse(Data("""
        {"value":{"run":"\(run)","boot":"\(boot)","publication":\(publication),
         "wall_ns":1000500000000,"segment":1,"memory_free_bytes":4294967296,
         "memory_total_bytes":51539607552,"pressure":2}}
        """.utf8))
    let selected = quantitySelection(selectedPoint).selected!
    let selection = Selection(second: 1000, selected: selected, alternatives: [selected],
        summary: other, trends: [other, selectedPoint], contextSeconds: 900)

    let data = MemoryQuantitiesData(selection: selection)

    precondition(data.rows.map(\.reading) == Array(repeating: "Unavailable", count: 13))
    precondition(data.identity == selected.identity)
    precondition(data.date == Date(timeIntervalSince1970: 1000.5))
}

private func keeps_unknown_pressure_unavailable() throws {
    // Goal: an unrecognized pressure code cannot become a guessed macOS verdict.
    // Method: project a literal code outside the three source states.
    let point = try quantityPoint(fields: "\"pressure\":99")

    let data = MemoryQuantitiesData(selection: quantitySelection(point))

    precondition(data.rows[12].reading == "Unavailable")
}

private func quantityPoint(fields: String) throws -> Metric {
    try decodeRecordingResponse(Data("""
        {"value":{"run":"selected","boot":"boot","publication":7,
         "wall_ns":1000500000000,"segment":1,\(fields)}}
        """.utf8))
}

@MainActor
private func quantityRenderGuard(field: String) throws {
    // Goal: the expanded actual Memory section paints availability and changing measured amounts.
    // Method: vary only one decoded field, keeping every chart source and identity unchanged.
    let measured = quantitySelection(try quantityPoint(fields: "\"\(field)\":4294967296"))
    let missing = quantitySelection(try quantityPoint(fields: "\"\(field)\":null"))
    let larger = quantitySelection(try quantityPoint(fields: "\"\(field)\":8589934592"))

    let measuredImage = quantityMemoryImage(measured)
    let availabilityDifferences = quantityImageDifferences(measuredImage, quantityMemoryImage(missing))
    let magnitudeDifferences = quantityImageDifferences(measuredImage, quantityMemoryImage(larger))

    precondition(availabilityDifferences > 40,
        "The actual Memory view ignored the expanded \(field) reading")
    print("Actual expanded Memory renders selected \(field): \(availabilityDifferences) availability pixels")
    precondition(magnitudeDifferences > 10,
        "The actual Memory view ignored the changed \(field) magnitude")
    print("Actual expanded Memory changes selected \(field): \(magnitudeDifferences) magnitude pixels")
}

@MainActor
private func quantityMemoryImage(_ selection: Selection) -> NSBitmapImageRep {
    let theme = FlightdeckTheme()
    let model = RecordingModel(path: "", pollingEnabled: false)
    model.selection = selection
    let renderer = ImageRenderer(content: RecordedMemory().environmentObject(model)
        .frame(width: 1080, height: 1500, alignment: .top)
        .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
        .background(theme.background).preferredColorScheme(.dark))
    renderer.scale = 1
    guard let image = renderer.cgImage else { preconditionFailure("Memory did not render") }

    return NSBitmapImageRep(cgImage: image)
}

private func quantityImageDifferences(_ first: NSBitmapImageRep,
                                      _ second: NSBitmapImageRep) -> Int {
    precondition(first.pixelsWide == 1080)
    precondition(first.pixelsHigh == 1500)
    precondition(second.pixelsWide == first.pixelsWide)
    precondition(second.pixelsHigh == first.pixelsHigh)
    precondition(first.bitsPerSample == 8)
    precondition(second.bitsPerSample == first.bitsPerSample)
    precondition(first.samplesPerPixel == 4)
    precondition(second.samplesPerPixel == first.samplesPerPixel)
    precondition(first.bytesPerRow >= 1080 * 4)
    precondition(first.bytesPerRow <= 16_384)
    precondition(second.bytesPerRow == first.bytesPerRow)
    guard let source = first.bitmapData, let target = second.bitmapData else {
        preconditionFailure("Memory has no bitmap pixel data")
    }
    var differences = 0

    for y in 0..<1500 {
        for x in 0..<1080 {
            let offset = y * first.bytesPerRow + x * 4
            let distance = abs(Int(source[offset]) - Int(target[offset]))
                + abs(Int(source[offset + 1]) - Int(target[offset + 1]))
                + abs(Int(source[offset + 2]) - Int(target[offset + 2]))
                + abs(Int(source[offset + 3]) - Int(target[offset + 3]))
            if distance > 15 { differences += 1 }
        }
    }

    return differences
}

private func keeps_all_six_measured_zeros_available() throws {
    // Goal: every new source quantity treats a measured zero as available.
    // Method: decode six literal zero quantities and the Critical macOS verdict.
    let point = try quantityPoint(fields: #"""
        "memory_free_bytes":0,"memory_inactive_bytes":0,"memory_speculative_bytes":0,
        "memory_anonymous_bytes":0,"memory_file_backed_bytes":0,"memory_purgeable_bytes":0,
        "pressure":2
        """#)

    let data = MemoryQuantitiesData(selection: quantitySelection(point))

    precondition(Array(data.rows[4...9]).map(\.reading) == Array(repeating: "0 GiB", count: 6))
    precondition(data.rows[12].reading == "Critical")
}

private func keeps_a_present_summary_without_quantities_unavailable() throws {
    // Goal: a present selected summary cannot invent absent quantity or pressure readings.
    // Method: decode explicit null quantities beside absent categories at the selected identity.
    let point = try quantityPoint(fields: #"""
        "memory_free_bytes":null,"memory_speculative_bytes":null,
        "memory_file_backed_bytes":null,"pressure":null
        """#)

    let data = MemoryQuantitiesData(selection: quantitySelection(point))

    precondition(data.rows.map(\.reading) == Array(repeating: "Unavailable", count: 13))
    precondition(data.identity == point.identity)
    precondition(data.date == Date(timeIntervalSince1970: 1000.5))
}
