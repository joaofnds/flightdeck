import AppKit
import Foundation
import SwiftUI

@MainActor
func runFanHistoryChecks() throws {
    try decodes_independent_literal_fan_gauges()
    try decodes_the_bounded_fan_array_independently()
    try preserves_old_absence_unread_and_measured_fanless_arrays()
    try reports_unavailable_rpm_for_nonempty_all_null_selected_channels()
    try gaps_negative_and_configured_nonfinite_gauges()
    try rejects_an_eleventh_channel_before_projecting_it()
    try rejects_a_nonarray_fan_payload()
    try rejects_a_string_channel_value()
    try rejects_a_boolean_channel_value()
    try rejects_an_unrepresentable_json_number()
    try projects_independent_channels_and_segment_gaps()
    try retains_exact_repeated_second_selection_and_honest_status()
    try uses_the_actual_rpm_chart_contract_and_semantic_roles()
    try uses_distinct_bounded_fan_strokes_in_the_actual_chart()
    try accepts_the_full_observation_channel_bound_and_refuses_the_next_point()
    try paints_changing_fan_rpm_gaps_and_supplied_role_in_actual_energy()
}

private func decodes_independent_literal_fan_gauges() throws {
    // Goal: the real metric decoder retains independent channel gauges, measured zero and gaps.
    // Method: decode a literal source array without deriving RPM from counters or another channel.
    let point: Metric = try decodeRecordingResponse(Data(#"""
        {"value":{"run":"run","boot":"boot","publication":1,
         "wall_ns":1000000000000,"segment":1,"fan_rpm":[0,1234.5,null,9876]}}
        """#.utf8))

    precondition(point.fanRpm?.values == [0, 1234.5, nil, 9876])
    print("Fan history decodes independent literal RPM gauges, zero and a missing channel")
}

private func decodes_the_bounded_fan_array_independently() throws {
    // Goal: the bounded fan value decodes literal independent channel readings itself.
    // Method: use its Decodable entry point with ten distinct channel slots.
    let readings = try JSONDecoder().decode(FanRPMReadings.self,
        from: Data("[0,1,2,3,4,5,6,7,8,9]".utf8))

    precondition(readings.values == [0, 1, 2, 3, 4, 5, 6, 7, 8, 9])
}

private func preserves_old_absence_unread_and_measured_fanless_arrays() throws {
    // Goal: older, unread and measured fanless observations remain distinct.
    // Method: decode all three public shapes before projecting their empty channel histories.
    let older = try fanPoint(nil)
    let unread = try fanPoint("null")
    let fanless = try fanPoint("[]")

    precondition(older.fanRpm == nil)
    precondition(unread.fanRpm == nil)
    precondition(fanless.fanRpm?.values == [])
    precondition(fanData([older]).lines.isEmpty)
    precondition(fanData([unread]).selectedMessage
        == "Fan readings unavailable at the selected observation.")
    precondition(fanData([fanless]).selectedMessage
        == "No fans reported at the selected observation.")
}

private func reports_unavailable_rpm_for_nonempty_all_null_selected_channels() throws {
    // Goal: reported channels with no valid RPM remain distinct from a fanless observation.
    // Method: project a selected nonempty nullable array through the real metric decoder.
    let point = try fanPoint("[null,null]")

    let data = fanData([point])

    precondition(data.selectedMessage
        == "RPM unavailable for all reported fan channels at the selected observation.")
}

private func gaps_negative_and_configured_nonfinite_gauges() throws {
    // Goal: invalid numeric readings do not invalidate independent valid channels.
    // Method: decode negatives through Metric and nonfinite numeric values through a configured decoder.
    let point = try fanPoint("[-1,0,2345.5]")
    let decoder = JSONDecoder()
    decoder.nonConformingFloatDecodingStrategy = .convertFromString(
        positiveInfinity: "Infinity", negativeInfinity: "-Infinity", nan: "NaN")

    let readings = try decoder.decode(FanRPMReadings.self,
        from: Data(#"["NaN","Infinity","-Infinity",-5,-0,1500]"#.utf8))

    precondition(point.fanRpm?.values == [nil, 0, 2345.5])
    precondition(readings.values == [nil, nil, nil, nil, 0, 1500])
    precondition(readings.values[4]?.sign == .plus)
}

private func rejects_an_eleventh_channel_before_projecting_it() throws {
    // Goal: an eleventh slot is refused even when every gauge is null.
    // Method: send an oversized nullable array through the real public Metric decoder.
    try fanRejects("[null,null,null,null,null,null,null,null,null,null,null]")
}

private func rejects_a_nonarray_fan_payload() throws {
    // Goal: a channel object cannot masquerade as an indexed gauge array.
    // Method: decode a literal object through the real public Metric decoder.
    try fanRejects(#"{"0":1234}"#)
}

private func rejects_a_string_channel_value() throws {
    // Goal: malformed textual gauges cannot silently become unavailable numeric readings.
    // Method: decode a string inside an otherwise well-formed channel array.
    try fanRejects(#"["1234"]"#)
}

private func rejects_a_boolean_channel_value() throws {
    // Goal: a boolean cannot become a measured zero or one RPM gauge.
    // Method: decode a literal boolean channel through the real public Metric decoder.
    try fanRejects("[true]")
}

private func rejects_an_unrepresentable_json_number() throws {
    // Goal: standard JSON decoding refuses an overflowing numeric representation.
    // Method: decode a numeric token larger than finite Double values permit.
    try fanRejects("[1e999]")
}

private func fanRejects(_ payload: String) throws {
    do {
        let point = try fanPoint(payload)
        preconditionFailure("Invalid fan payload decoded as \(point.id)")
    } catch is DecodingError {
        return
    }
}

private func projects_independent_channels_and_segment_gaps() throws {
    // Goal: channels retain gauge values independently across null, fanless and identity changes.
    // Method: project literal arrays with increasing channel counts and changed source segments.
    let points = [try fanPoint("[0,1234.5,null]"),
        try fanPoint("[4321,null,900]", publication: 2, second: 1001),
        try fanPoint("[]", publication: 3, second: 1002),
        try fanPoint("[2000,3000]", publication: 4, second: 1003, segment: 2),
        try fanPoint("[2100,3100]", publication: 1, second: 1004, run: "new"),
        try fanPoint("[2200,3200]", publication: 2, second: 1005, run: "new", boot: "new")]

    let data = fanData(points)
    let plot = data.context.plot(lines: data.lines)

    precondition(data.lines.map(\.name) == ["Fan 0", "Fan 1", "Fan 2"])
    precondition(data.lines.map(\.values) == [[0,4321,nil,2000,2100,2200],
        [1234.5,nil,nil,3000,3100,3200], [nil,900,nil,nil,nil,nil]])
    precondition(data.lines.map(\.unit) == ["RPM", "RPM", "RPM"])
    precondition(data.lines.allSatisfy { $0.negative == false })
    precondition(data.lines.allSatisfy { $0.stack == false })
    precondition(plot?.segments(line: 0) == [[0,1], [3], [4], [5]])
    precondition(plot?.segments(line: 1) == [[0], [3], [4], [5]])
    precondition(plot?.segments(line: 2) == [[1]])
    precondition(data.context.domain == 106...1006)
    precondition(plot?.selectedTime(id: points[5].id) == 1005)
    precondition(data.context.target(id: points[5].id)?.identity == points[5].identity)
    print("Fan projection preserves independent gauges, channel counts and source segment gaps")
}

private func retains_exact_repeated_second_selection_and_honest_status() throws {
    // Goal: repeated-second selection retains the exact source and a gap borrows no current readings.
    // Method: select an earlier fanless observation beside a later same-second measured sibling.
    let selected = try fanPoint("[]")
    let sibling = try fanPoint("[50,75]", publication: 2)
    let selection = fanSelection([selected, sibling], selected: selected)
    let gap = Selection(second: 1000, selected: nil, alternatives: [], summary: sibling,
        trends: [selected, sibling], contextSeconds: 900)
    let mismatched = Selection(second: 1000, selected: selection.selected,
        alternatives: selection.alternatives, summary: sibling,
        trends: [selected, sibling], contextSeconds: 900)

    let data = FanHistoryData(context: RecordedChartContext(selection: selection),
        theme: FlightdeckTheme())!

    precondition(data.context.point(at: 1000.5)?.identity == selected.identity)
    precondition(data.context.target(id: selected.id)?.identity == selected.identity)
    precondition(data.selectedMessage == "No fans reported at the selected observation.")
    precondition(FanHistoryData(context: RecordedChartContext(selection: gap),
        theme: FlightdeckTheme())?.selectedMessage
        == "Fan readings unavailable at the selected observation.")
    precondition(FanHistoryData(context: RecordedChartContext(selection: mismatched),
        theme: FlightdeckTheme())?.selectedMessage
        == "Fan readings unavailable at the selected observation.")
}

@MainActor
private func uses_the_actual_rpm_chart_contract_and_semantic_roles() throws {
    // Goal: the production fan chart uses RPM, nonnegative unstacked axes and exact positive readouts.
    // Method: build the actual chart constructor from literal gauges and replace its semantic colors.
    let point = try fanPoint("[50,75]")
    var theme = FlightdeckTheme()
    theme.fanSeries[0] = .pink
    theme.fanSeries[1] = .cyan
    let data = FanHistoryData(context: RecordedChartContext(selection: fanSelection([point])),
        theme: theme)!

    let chart = RecordedFanHistory.chart(data,
        inspectedID: .constant(nil), inspecting: .constant(false))

    precondition(data.lines.map(\.color) == [.pink, .cyan])
    precondition(chart.title == "Fan speed")
    precondition(chart.style.unit == "RPM")
    precondition(chart.style.minimum == 0)
    precondition(chart.style.stack == false)
    precondition(chart.reading(chart.lines[0]) == "50 RPM")
    precondition(chart.reading(chart.lines[1]) == "75 RPM")
    print("Actual fan chart uses independent semantic roles and positive literal RPM readouts")
}

@MainActor
private func uses_distinct_bounded_fan_strokes_in_the_actual_chart() throws {
    // Goal: all ten labeled fan channels retain distinct plot and legend stroke cadences.
    // Method: inspect the actual chart lines through the shared plot and legend stroke API.
    let point = try fanPoint("[1000,1100,1200,1300,1400,1500,1600,1700,1800,1900]")
    let data = fanData([point])
    let chart = RecordedFanHistory.chart(data,
        inspectedID: .constant(nil), inspecting: .constant(false))

    let styles = chart.lines.map { $0.strokePattern.style(width: 2.5) }

    precondition(styles.map(\.dash) == [[], [5,4], [1,4], [12,4], [8,3,2,3],
        [8,3,2,3,2,3], [3,2], [12,3,4,3], [3,2,3,2,3,6], [1,2,1,6]])
    precondition(styles.map(\.lineCap) == [.butt, .butt, .round, .butt, .butt,
        .butt, .butt, .butt, .butt, .butt])
    precondition(styles.allSatisfy { $0.lineWidth == 2.5 })
    print("Actual fan chart retains ten distinct bounded plot and legend stroke cadences")
}

private func accepts_the_full_observation_channel_bound_and_refuses_the_next_point() throws {
    // Goal: projection remains inside 4096 observations by ten channels and refuses the next point.
    // Method: decode unique exact identities in one shared wall second before requesting both sizes.
    let points = try (0..<4096).map { index in
        try fanPoint("[0,1,2,3,4,5,6,7,8,9]", publication: UInt64(index) + 1)
    }
    let extra = try fanPoint("[0,1,2,3,4,5,6,7,8,9]", publication: 4097)
    let oversized = fanSelection(points + [extra])

    let data = fanData(points)

    precondition(data.lines.count == 10)
    precondition(data.lines.allSatisfy { $0.values.count == 4096 })
    precondition(data.context.plot(lines: data.lines) != nil)
    precondition(FanHistoryData(context: RecordedChartContext(selection: oversized),
        theme: FlightdeckTheme()) == nil)
    print("Native fan projection accepts 4096 by ten cells and rejects 4097 observations")
}

@MainActor
private func paints_changing_fan_rpm_gaps_and_supplied_role_in_actual_energy() throws {
    // Goal: actual Energy paints fan magnitudes, unavailable gaps and the supplied semantic color.
    // Method: vary Fan 0 alone and reuse its measured image to inspect the bright green plot stroke.
    let measured = try fanSustainedSelection("[1000,3000]")
    let changed = try fanSustainedSelection("[2000,3000]")
    let unavailable = try fanSustainedSelection("[null,3000]")

    let measuredImage = fanEnergyImage(measured)
    let magnitude = fanImageDifferences(measuredImage, fanEnergyImage(changed))
    let availability = fanImageDifferences(measuredImage, fanEnergyImage(unavailable))
    let green = fanGreenPlotPixels(measuredImage)

    precondition(magnitude > 500, "Actual Energy ignored the changed fan RPM plot")
    precondition(availability > 500, "Actual Energy did not leave a gap for unavailable fan RPM")
    print("Actual Energy fan role probe: \(green) green plot pixels")
    precondition(green > 500, "Actual Energy ignored the supplied fan series color")
    print("Actual Energy fan plot changes \(magnitude) magnitude and \(availability) availability pixels")
}

@MainActor
private func fanEnergyImage(_ selection: Selection) -> NSBitmapImageRep {
    var theme = FlightdeckTheme()
    theme.fanSeries[0] = Color(red: 0, green: 1, blue: 0)
    let model = RecordingModel(path: "", pollingEnabled: false)
    model.selection = selection
    let renderer = ImageRenderer(content: RecordedComponentPane(component: .energy)
        .environmentObject(model).frame(width: 1080, height: 1200, alignment: .top)
        .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
        .background(theme.background).preferredColorScheme(.dark))
    renderer.scale = 1
    guard let image = renderer.cgImage else { preconditionFailure("Energy did not render") }

    return NSBitmapImageRep(cgImage: image)
}

private func fanImageDifferences(_ first: NSBitmapImageRep,
                                  _ second: NSBitmapImageRep) -> Int {
    precondition(first.pixelsWide == 1080)
    precondition(first.pixelsHigh == 1200)
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
        preconditionFailure("Energy has no bitmap pixel data")
    }
    var differences = 0

    // Interior columns exclude legend swatches, channel names, readouts and axis numbers.
    for y in 0..<1200 {
        for x in 140..<900 {
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

private func fanGreenPlotPixels(_ image: NSBitmapImageRep) -> Int {
    precondition(image.pixelsWide == 1080)
    precondition(image.pixelsHigh == 1200)
    precondition(image.bitsPerSample == 8)
    precondition(image.samplesPerPixel == 4)
    precondition(image.bitmapFormat.contains(.alphaFirst) == false)
    precondition(image.bytesPerRow >= 1080 * 4)
    precondition(image.bytesPerRow <= 16_384)
    guard let pixels = image.bitmapData else {
        preconditionFailure("Energy has no bitmap pixel data")
    }
    var count = 0

    // Interior columns exclude legend colors and text. Count saturated green stroke pixels.
    for y in 0..<1200 {
        for x in 140..<900 {
            let offset = y * image.bytesPerRow + x * 4
            if pixels[offset] < 80 && pixels[offset + 1] > 200 && pixels[offset + 2] < 80 {
                count += 1
            }
        }
    }

    return count
}

private func fanData(_ points: [Metric]) -> FanHistoryData {
    FanHistoryData(context: RecordedChartContext(selection: fanSelection(points)),
        theme: FlightdeckTheme())!
}

private func fanPoint(_ payload: String?, publication: UInt64 = 1, second: UInt64 = 1000,
                      segment: UInt64 = 1, run: String = "run", boot: String = "boot") throws -> Metric {
    let field = payload.map { ",\"fan_rpm\":\($0)" } ?? ""
    return try decodeRecordingResponse(Data("""
        {"value":{"run":"\(run)","boot":"\(boot)","publication":\(publication),
         "wall_ns":\(second * 1_000_000_000),"segment":\(segment),
         "power_watts":10,"gpu_watts":5,"soc_celsius":50\(field)}}
        """.utf8))
}

private func fanSelection(_ points: [Metric], selected: Metric? = nil) -> Selection {
    let point = selected ?? points.last!
    let observation = Observation(run: point.run, boot: point.boot,
        publication: point.publication, runTick: 1, wallNs: point.wallNs, windowLosses: nil)

    return Selection(second: points.last!.wallNs / 1_000_000_000, selected: observation,
        alternatives: [observation], summary: point, trends: points, contextSeconds: 900)
}

private func fanSustainedSelection(_ payload: String) throws -> Selection {
    let points = try (0..<900).map { index in
        try fanPoint(payload, publication: UInt64(index) + 1, second: UInt64(index) + 1000)
    }

    return fanSelection(points)
}
