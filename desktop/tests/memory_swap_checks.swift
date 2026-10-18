import AppKit
import Foundation
import SwiftUI

@MainActor
func runMemorySwapChecks() throws {
    try preserves_decimal_swap_rates_and_independent_missing_directions()
    try preserves_exact_identity_and_shared_segment_context()
    try reports_actual_swap_chart_units_and_positive_write_readouts()
    try paints_actual_memory_swap_directions_with_independent_roles()
    try paints_measured_swap_zero_and_leaves_unavailable_gaps()
}

private func preserves_decimal_swap_rates_and_independent_missing_directions() throws {
    // Goal: independent raw byte rates become nonnegative decimal MB/s with honest zero and gaps.
    // Method: decode literal mixed directions before projecting the actual Memory series.
    let points: [Metric] = try decodeRecordingResponse(Data(#"""
        {"value":[
          {"run":"run","boot":"boot","publication":1,"wall_ns":1000000000000,
           "segment":1,"swap_in_bytes_per_second":1250000,
           "swap_out_bytes_per_second":3500000},
          {"run":"run","boot":"boot","publication":2,"wall_ns":1001000000000,
           "segment":1,"swap_in_bytes_per_second":0,"swap_out_bytes_per_second":null},
          {"run":"run","boot":"boot","publication":3,"wall_ns":1002000000000,
           "segment":1,"swap_in_bytes_per_second":null,"swap_out_bytes_per_second":0},
          {"run":"run","boot":"boot","publication":4,"wall_ns":1003000000000,
           "segment":2},
          {"run":"run","boot":"boot","publication":5,"wall_ns":1004000000000,
           "segment":2,"swap_in_bytes_per_second":9876543,
           "swap_out_bytes_per_second":1000000}]}
        """#.utf8))
    let selection = swapSelection(points)

    let data = RecordedMemoryData(selection: selection, theme: FlightdeckTheme())
    let plot = data.plot(lines: data.swapTraffic)

    precondition(data.swapTraffic.map(\.name) == ["Read from swap", "Write to swap"])
    precondition(data.swapTraffic.map(\.values) == [
        [1.25, 0, nil, nil, 9.876543], [3.5, nil, 0, nil, 1]])
    precondition(data.swapTraffic[0].negative != true)
    precondition(data.swapTraffic[1].negative == true)
    precondition(plot?.segments(line: 0) == [[0, 1], [4]])
    precondition(plot?.segments(line: 1) == [[0], [2], [4]])
    print("Swap traffic retains decimal MB/s, positive readouts, independent zero and missing rates")
}

private func swapSelection(_ points: [Metric], selected: Metric? = nil) -> Selection {
    let point = selected ?? points.last!
    let observation = Observation(run: point.run, boot: point.boot,
        publication: point.publication, runTick: 1, wallNs: point.wallNs, windowLosses: nil)

    return Selection(second: points.last!.wallNs / 1_000_000_000, selected: observation,
        alternatives: [observation], summary: point, trends: points, contextSeconds: 900)
}

private func preserves_exact_identity_and_shared_segment_context() throws {
    // Goal: swap traffic uses the same exact alternatives, wall domain and discontinuities as Memory.
    // Method: decode repeated seconds plus changed run, boot and segment identities.
    let points: [Metric] = try decodeRecordingResponse(Data(#"""
        {"value":[
          {"run":"a","boot":"a","publication":1,"wall_ns":1000000000000,
           "segment":1,"swap_in_bytes_per_second":1000000,"swap_out_bytes_per_second":2000000},
          {"run":"b","boot":"a","publication":1,"wall_ns":1000000000000,
           "segment":1,"swap_in_bytes_per_second":3000000,"swap_out_bytes_per_second":4000000},
          {"run":"b","boot":"a","publication":2,"wall_ns":1001000000000,
           "segment":1,"swap_in_bytes_per_second":5000000,"swap_out_bytes_per_second":6000000},
          {"run":"b","boot":"b","publication":3,"wall_ns":1002000000000,
           "segment":1,"swap_in_bytes_per_second":7000000,"swap_out_bytes_per_second":8000000},
          {"run":"b","boot":"b","publication":4,"wall_ns":1003000000000,
           "segment":2,"swap_in_bytes_per_second":9000000,"swap_out_bytes_per_second":10000000}]}
        """#.utf8))
    let data = RecordedMemoryData(selection: swapSelection(points, selected: points[1]),
        theme: FlightdeckTheme())

    let plot = data.plot(lines: data.swapTraffic)
    let categories = data.plot(lines: data.categories)

    precondition(plot?.samples == categories?.samples)
    precondition(plot?.domain == 104...1004)
    precondition(plot?.segments(line: 0) == [[0], [1, 2], [3], [4]])
    precondition(plot?.segments(line: 1) == [[0], [1, 2], [3], [4]])
    precondition(plot?.selectedTime(id: data.selectedID!) == 1000)
    precondition(data.context.point(at: 1000.5)?.identity == points[1].identity)
    precondition(data.context.target(id: points[1].id) == RecordedSelectionTarget(second: 1000,
        identity: RecordedObservationIdentity(run: "b", boot: "a", publication: 1)))
    precondition(data.context.target(id: "unavailable") == nil)
    precondition(data.context.point(at: 999.5) == nil)
    print("Swap traffic shares the 900-second domain, exact alternatives and segment boundaries")
}

@MainActor
private func paints_actual_memory_swap_directions_with_independent_roles() throws {
    // Goal: the actual Memory view paints both role-controlled traces with writes below reads.
    // Method: render sustained real DTOs with isolated green and magenta swap role replacements.
    let selection = try swapSustainedSelection()
    var theme = FlightdeckTheme()
    theme.swapReadSeries = Color(red: 0, green: 1, blue: 0)
    theme.swapWriteSeries = Color(red: 1, green: 0, blue: 1)

    let image = swapMemoryImage(selection, theme: theme)
    let read = swapStrokePixels(image, green: true)
    let write = swapStrokePixels(image, green: false)

    precondition(read.count > 500, "The actual Memory chart did not paint the swap read role")
    precondition(write.count > 500, "The actual Memory chart did not paint the swap write role")
    precondition(write.meanY > read.meanY + 40, "The actual swap write trace was not mirrored")
    print("Actual Memory paints independent swap roles and mirrors write geometry below reads")
}

@MainActor
private func paints_measured_swap_zero_and_leaves_unavailable_gaps() throws {
    // Goal: measured zero remains a painted swap trace while unavailable traffic stays blank.
    // Method: compare the real Memory plot interior for 900 measured-zero and missing read samples.
    let zero = try swapSustainedSelection(read: "0", write: "null")
    let missing = try swapSustainedSelection(read: "null", write: "null")
    var theme = FlightdeckTheme()
    theme.swapReadSeries = Color(red: 0, green: 1, blue: 0)
    theme.swapWriteSeries = Color(red: 1, green: 0, blue: 1)

    let zeroPixels = swapStrokePixels(swapMemoryImage(zero, theme: theme), green: true)
    let missingPixels = swapStrokePixels(swapMemoryImage(missing, theme: theme), green: true)

    precondition(zeroPixels.count > 500, "Measured zero disappeared from the actual swap chart")
    precondition(missingPixels.count == 0, "Unavailable swap reads painted a fabricated trace")
    print("Actual Memory paints a zero swap rate and leaves unavailable swap rates blank")
}

@MainActor
private func swapMemoryImage(_ selection: Selection,
                             theme: FlightdeckTheme) -> NSBitmapImageRep {
    let model = RecordingModel(path: "", pollingEnabled: false)
    model.selection = selection
    let renderer = ImageRenderer(content: RecordedMemory().environmentObject(model)
        .frame(width: 1080, height: 1100, alignment: .top)
        .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
        .background(theme.background).preferredColorScheme(.dark))
    renderer.scale = 1
    guard let image = renderer.cgImage else { preconditionFailure("Memory did not render") }

    return NSBitmapImageRep(cgImage: image)
}

private func swapStrokePixels(_ image: NSBitmapImageRep,
                              green: Bool) -> (count: Int, meanY: Double) {
    precondition(image.pixelsWide == 1080)
    precondition(image.pixelsHigh == 1100)
    var count = 0
    var totalY = 0

    // Interior columns exclude legend swatches, readout text, and the colored pressure legend.
    for y in 120..<1000 {
        for x in 120..<900 {
            guard let color = image.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else {
                preconditionFailure("Memory has no RGB plot pixel")
            }

            let matches = green
                ? color.greenComponent > 0.8 && color.redComponent < 0.3 && color.blueComponent < 0.3
                : color.redComponent > 0.8 && color.blueComponent > 0.8 && color.greenComponent < 0.3
            if matches { count += 1; totalY += y }
        }
    }

    return (count: count, meanY: count == 0 ? 0 : Double(totalY) / Double(count))
}

private func swapSustainedSelection(read: String = "1250000",
                                    write: String = "3500000") throws -> Selection {
    let rows = (0..<900).map { index in
        """
        {"run":"run","boot":"boot","publication":\(index + 1),
         "wall_ns":\((1000 + UInt64(index)) * 1_000_000_000),"segment":1,
         "swap_in_bytes_per_second":\(read),"swap_out_bytes_per_second":\(write)}
        """
    }.joined(separator: ",")
    let points: [Metric] = try decodeRecordingResponse(Data("{\"value\":[\(rows)]}".utf8))

    return swapSelection(points)
}

@MainActor
private func reports_actual_swap_chart_units_and_positive_write_readouts() throws {
    // Goal: the actual Memory chart reports positive write traffic in decimal MB/s.
    // Method: use its production constructor and readout with literal source byte rates.
    let selection = try swapSustainedSelection()
    let data = RecordedMemoryData(selection: selection, theme: FlightdeckTheme())

    let chart = RecordedMemory.swapTrafficChart(data,
        inspectedID: .constant(nil), inspecting: .constant(false))
    let reading = chart.reading(chart.lines[1]).replacingOccurrences(of: ",", with: ".")

    precondition(chart.title == "Swap traffic")
    precondition(chart.style.unit == "MB/s", "The actual swap chart uses the wrong byte-rate unit")
    precondition(reading == "3.5 MB/s", "The actual swap write readout lost its positive decimal value")
    print("Actual Memory chart reports the literal positive write readout 3.5 MB/s")
}
