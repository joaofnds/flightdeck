import Combine
import Foundation
import SwiftUI

@MainActor
func runOverviewInspectionChecks() throws {
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

func runRecordedComponentChecks() throws {
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
    print("Dashboard selection, readings, schematic motion and bounded time-series checks passed")
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
