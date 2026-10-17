import Foundation

func runCPUHistoryChecks() throws {
    try preserves_independent_cpu_history_values_and_exact_context()
    try rejects_a_predecessor_that_changes_an_existing_context_observation()
    try decodes_exact_publications_and_independent_values_from_the_wire()
    try preserves_missing_values_across_an_incompatible_cpu_interval()
    try accepts_an_interval_whose_predecessor_is_before_the_chart_domain()
    try accepts_an_empty_cpu_domain()
    try preserves_every_cpu_across_full_and_partial_pages()
    try accepts_the_bounded_dense_history_and_rejects_one_more_observation()
    try keeps_a_bounded_restoration_notice_beside_available_history()
    for message in ["", String(repeating: "x", count: 513)] {
        try rejects_an_invalid_restoration_notice(message)
    }

    for (name, changed) in cpuHistoryObservationVariants(cpuHistoryObservation(11)) {
        try rejects_changed_selected_observation(changed, caseName: "selected \(name)")
    }

    for (name, changed) in cpuHistoryObservationVariants(cpuHistoryObservation(10)) {
        try rejects_changed_common_observation(changed, caseName: "common \(name)")
        try rejects_changed_interval_predecessor(changed, caseName: "predecessor \(name)")
    }

    for (name, payload, error) in cpuHistoryInvalidPages() {
        try rejectsCPUHistory(payload, error: error, caseName: name)
    }

    for (name, value) in [
        ("NaN", Double.nan), ("positive infinity", Double.infinity),
        ("negative infinity", -Double.infinity),
        ("below zero", -Double.leastNonzeroMagnitude), ("above 100", 100.0.nextUp)] {
        try rejects_invalid_history_percentage(value, caseName: name)
    }

    try rejects_a_changed_cpu_domain_for_an_available_interval()
    try rejects_a_value_in_an_unavailable_interval()
}

private func keeps_a_bounded_restoration_notice_beside_available_history() throws {
    // Goal: a CPU-local restoration error keeps readings visible and makes retries eligible.
    // Method: validate a full-width Unicode diagnostic alongside the ordinary history fixture.
    var payload = cpuHistoryPayload()
    payload.restorationError = String(repeating: "温", count: 512)

    let history = try SelectedCPUHistory(payload: payload, selection: cpuHistorySelection())
    let reading = CPUHistoryRead.readings(history)

    precondition(reading.notice == payload.restorationError)
    precondition(reading.readings?.cpus[0].busyPercent == payload.cpus[0].busyPercent)
}

private func rejects_an_invalid_restoration_notice(_ message: String) throws {
    // Goal: transport diagnostics stay nonempty and bounded before reaching the UI.
    // Method: reject an empty or oversized error without rejecting valid history values.
    var payload = cpuHistoryPayload()
    payload.restorationError = message

    try rejectsCPUHistory(payload, error: .diagnostic, caseName: "restoration diagnostic")
}

private func decodes_exact_publications_and_independent_values_from_the_wire() throws {
    // Goal: transport decoding preserves exact identities above Double's integer precision.
    // Method: decode a real snake-case envelope with two publications in one wall second.
    let data = Data(#"""
        {"value":{"observation":{"run":"run","boot":"boot",
        "publication":9007199254740994,"run_tick":11,"wall_ns":1001200000000},
        "context_seconds":900,"first_ns":102000000000,"end_ns":1002000000000,
        "offset":0,"total":2,"retention_limited":true,
        "restoration_error":"Earlier CPU history temporarily unreadable","observations":[
        {"observation":{"run":"run","boot":"boot","publication":9007199254740993,
        "run_tick":10,"wall_ns":1001100000000},"previous_observation":{
        "run":"run","boot":"boot","publication":9007199254740992,
        "run_tick":9,"wall_ns":1000900000000},"interval_available":true,"cpu_count":2},
        {"observation":{"run":"run","boot":"boot","publication":9007199254740994,
        "run_tick":11,"wall_ns":1001200000000},"previous_observation":{
        "run":"run","boot":"boot","publication":9007199254740993,
        "run_tick":10,"wall_ns":1001100000000},"interval_available":true,"cpu_count":2}],
        "cpus":[{"cpu":0,"busy_percent":[0,null]},{"cpu":1,"busy_percent":[null,100]}]}}
        """#.utf8)
    let payload: CPUHistoryPayload = try decodeRecordingResponse(data)
    let points = [
        Observation(run: "run", boot: "boot", publication: 9_007_199_254_740_993,
            runTick: 10, wallNs: 1_001_100_000_000, windowLosses: nil),
        Observation(run: "run", boot: "boot", publication: 9_007_199_254_740_994,
            runTick: 11, wallNs: 1_001_200_000_000, windowLosses: nil)]
    let selection = try cpuHistorySelection(points: points, selected: points[1])

    let history = try SelectedCPUHistory(payload: payload, selection: selection)

    precondition(history.observations.map(\.observation) == points)
    precondition(history.retentionLimited)
    precondition(CPUHistoryRead.readings(history).notice
        == "Earlier CPU history temporarily unreadable")
    precondition(history.cpus[0].busyPercent == [0, nil])
    precondition(history.cpus[1].busyPercent == [nil, 100])
}

private func rejects_changed_selected_observation(_ changed: Observation,
                                                 caseName: String) throws {
    // Goal: another selected identity or time cannot borrow this CPU history.
    // Method: change one independently named field at the selected observation boundary.
    let payload = cpuHistoryPayload(observation: changed)

    try rejectsCPUHistory(payload, error: .observationMismatch, caseName: caseName)
}

private func rejects_changed_common_observation(_ changed: Observation,
                                               caseName: String) throws {
    // Goal: CPU history cannot use another common-chart identity or time.
    // Method: change one independently named field in the common-chart context.
    let selection = try cpuHistorySelection(points: [changed, cpuHistoryObservation(11)])

    try rejectsCPUHistory(cpuHistoryPayload(), error: .context, selection: selection,
        caseName: caseName)
}

private func preserves_missing_values_across_an_incompatible_cpu_interval() throws {
    // Goal: a changed CPU domain leaves a gap without erasing later CPU measurements.
    // Method: cross an unavailable interval, then retain an independent nil in a valid one.
    let first = cpuHistoryObservation(9)
    let second = cpuHistoryObservation(10)
    let points = [
        CPUHistoryObservation(observation: first, previousObservation: nil,
            intervalAvailable: false, cpuCount: 1),
        CPUHistoryObservation(observation: second, previousObservation: first,
            intervalAvailable: false, cpuCount: 2),
        CPUHistoryObservation(observation: cpuHistoryObservation(11), previousObservation: second,
            intervalAvailable: true, cpuCount: 2)]
    let payload = cpuHistoryPayload(points: points, cpus: [
        LogicalCPUHistoryRow(cpu: 0, busyPercent: [nil, nil, 0]),
        LogicalCPUHistoryRow(cpu: 1, busyPercent: [nil, nil, nil])])
    let selection = try cpuHistorySelection(points: points.map(\.observation))

    let history = try SelectedCPUHistory(payload: payload, selection: selection)

    precondition(history.cpus[0].busyPercent == [nil, nil, 0])
    precondition(history.cpus[1].busyPercent == [nil, nil, nil])
    precondition(history.observations.map(\.cpuCount) == [1, 2, 2])
}

private func accepts_an_empty_cpu_domain() throws {
    // Goal: an unavailable CPU domain remains an empty page rather than an invented CPU.
    // Method: validate the selected zero-CPU observation with no rows or measured intervals.
    let points = [10, 11].map { publication in
        CPUHistoryObservation(observation: cpuHistoryObservation(UInt64(publication)),
            previousObservation: nil, intervalAvailable: false, cpuCount: 0)
    }
    let selection = try cpuHistorySelection()

    let history = try SelectedCPUHistory(payload: cpuHistoryPayload(points: points, cpus: [],
        total: 0), selection: selection)

    precondition(history.total == 0)
    precondition(history.offset == 0)
    precondition(history.cpus.isEmpty)
}

private func accepts_an_interval_whose_predecessor_is_before_the_chart_domain() throws {
    // Goal: the first plotted CPU interval may begin before the chart's left edge.
    // Method: validate a left-edge observation using a predecessor one second outside it.
    let previous = Observation(run: "run", boot: "boot", publication: 9, runTick: 9,
        wallNs: 101_000_000_000, windowLosses: nil)
    let first = Observation(run: "run", boot: "boot", publication: 10, runTick: 10,
        wallNs: 102_000_000_000, windowLosses: nil)
    let selected = Observation(run: "run", boot: "boot", publication: 1000, runTick: 1000,
        wallNs: 1_001_000_000_000, windowLosses: nil)
    let selectedPrevious = Observation(run: "run", boot: "boot", publication: 999, runTick: 999,
        wallNs: 1_000_000_000_000, windowLosses: nil)
    let points = [
        CPUHistoryObservation(observation: first, previousObservation: previous,
            intervalAvailable: true, cpuCount: 2),
        CPUHistoryObservation(observation: selected, previousObservation: selectedPrevious,
            intervalAvailable: true, cpuCount: 2)]
    let selection = try cpuHistorySelection(points: [first, selected], selected: selected)
    let payload = cpuHistoryPayload(observation: selected, points: points)

    let history = try SelectedCPUHistory(payload: payload, selection: selection)

    precondition(history.observations.first?.observation.wallNs == history.firstNs)
    precondition(history.cpus[0].busyPercent == [0, 50])
    precondition(history.cpus[1].busyPercent == [nil, 100])
}

private func preserves_every_cpu_across_full_and_partial_pages() throws {
    // Goal: paging reaches each logical CPU once, including a partial final page.
    // Method: generate every supported domain size and visit all of its 32-row pages.
    let selection = try cpuHistorySelection()

    for total in UInt32(1)...UInt32(SelectedCPUReadings.cpusMax) {
        var found = [UInt32]()
        for offset in stride(from: UInt32(0), to: total, by: 32) {
            let rows = (offset..<min(offset + 32, total)).map { cpu in
                LogicalCPUHistoryRow(cpu: cpu, busyPercent: [0, 100])
            }
            let history = try SelectedCPUHistory(payload: cpuHistoryPayload(cpus: rows,
                offset: offset, total: total), selection: selection)

            precondition(history.cpus.count <= 32, "total \(total), offset \(offset)")
            found.append(contentsOf: history.cpus.map(\.cpu))
        }

        precondition(found == Array(0..<total), "CPU domain \(total)")
    }
}

private func accepts_the_bounded_dense_history_and_rejects_one_more_observation() throws {
    // Goal: the pure boundary supports 4096 observations and a full page of 1024 CPUs.
    // Method: place dense subsecond observations inside 900 seconds, then add the 4097th.
    let points = cpuHistoryDensePoints(count: SelectedCPUHistory.observationsMax)
    let selection = try cpuHistorySelection(points: points.map(\.observation),
        selected: points[points.count - 1].observation)
    let rows = (UInt32(992)..<UInt32(1024)).map { cpu in
        LogicalCPUHistoryRow(cpu: cpu, busyPercent: Array(repeating: 0, count: points.count))
    }
    let payload = cpuHistoryPayload(observation: points[points.count - 1].observation,
        points: points, cpus: rows, offset: 992, total: 1024)

    let history = try SelectedCPUHistory(payload: payload, selection: selection)

    precondition(history.observations.count * history.cpus.count == 4096 * 32)
    precondition(history.cpus.last?.cpu == 1023)

    let excessive = cpuHistoryDensePoints(count: SelectedCPUHistory.observationsMax + 1)
    let excessiveSelection = try cpuHistorySelection(points: excessive.map(\.observation),
        selected: excessive[excessive.count - 1].observation)
    let excessiveRows = rows.map { row in
        LogicalCPUHistoryRow(cpu: row.cpu, busyPercent: row.busyPercent + [0])
    }
    let excessivePayload = cpuHistoryPayload(observation: excessive[excessive.count - 1].observation,
        points: excessive, cpus: excessiveRows, offset: 992, total: 1024)

    try rejectsCPUHistory(excessivePayload, error: .context, selection: excessiveSelection)
}

private func cpuHistoryInvalidPages() -> [(String, CPUHistoryPayload, CPUHistoryError)] {
    [
        ("unaligned offset", cpuHistoryPayload(offset: 1), .page),
        ("offset past domain", cpuHistoryPayload(offset: 32), .page),
        ("missing rows", cpuHistoryPayload(total: 33), .page),
        ("extra rows", cpuHistoryPayload(total: 1), .page),
        ("zero domain with rows", cpuHistoryPayload(total: 0), .page),
        ("zero domain with offset", cpuHistoryPayload(cpus: [], offset: 32, total: 0), .page),
        ("domain beyond bound", cpuHistoryPayload(total: 1025), .domain),
        ("selected domain mismatch", cpuHistoryPayload(points: [
            CPUHistoryObservation(observation: cpuHistoryObservation(10),
                previousObservation: cpuHistoryObservation(9), intervalAvailable: true, cpuCount: 3),
            CPUHistoryObservation(observation: cpuHistoryObservation(11),
                previousObservation: cpuHistoryObservation(10), intervalAvailable: true,
                cpuCount: 3)]), .domain),
        ("extra partial-page row", cpuHistoryPayload(cpus: [
            LogicalCPUHistoryRow(cpu: 32, busyPercent: [0, 50]),
            LogicalCPUHistoryRow(cpu: 33, busyPercent: [nil, 100])], offset: 32, total: 33), .page),
        ("duplicate CPU", cpuHistoryPayload(cpus: [
            LogicalCPUHistoryRow(cpu: 0, busyPercent: [0, 50]),
            LogicalCPUHistoryRow(cpu: 0, busyPercent: [nil, 100])]), .domain),
        ("reordered CPUs", cpuHistoryPayload(cpus: [
            LogicalCPUHistoryRow(cpu: 1, busyPercent: [0, 50]),
            LogicalCPUHistoryRow(cpu: 0, busyPercent: [nil, 100])]), .domain),
        ("CPU past domain", cpuHistoryPayload(cpus: [
            LogicalCPUHistoryRow(cpu: 0, busyPercent: [0, 50]),
            LogicalCPUHistoryRow(cpu: 2, busyPercent: [nil, 100])]), .domain),
        ("missing cell", cpuHistoryPayload(cpus: [
            LogicalCPUHistoryRow(cpu: 0, busyPercent: [0]),
            LogicalCPUHistoryRow(cpu: 1, busyPercent: [nil, 100])]), .domain),
        ("extra nil cell", cpuHistoryPayload(cpus: [
            LogicalCPUHistoryRow(cpu: 0, busyPercent: [0, 50, nil]),
            LogicalCPUHistoryRow(cpu: 1, busyPercent: [nil, 100])]), .domain)]
}

private func rejects_invalid_history_percentage(_ value: Double, caseName: String) throws {
    // Goal: missing, zero and 100 remain distinct from nonfinite or out-of-range data.
    // Method: retain valid endpoints and an independent nil beside the named invalid value.
    let payload = cpuHistoryPayload(cpus: [
        LogicalCPUHistoryRow(cpu: 0, busyPercent: [0, value]),
        LogicalCPUHistoryRow(cpu: 1, busyPercent: [nil, 100])])

    try rejectsCPUHistory(payload, error: .percentage, caseName: caseName)
}

private func rejects_changed_interval_predecessor(_ changed: Observation,
                                                 caseName: String) throws {
    // Goal: an available value requires consecutive observations in one run and CPU domain.
    // Method: change one independently named predecessor field against a known observation.
    let points = [
        CPUHistoryObservation(observation: cpuHistoryObservation(10),
            previousObservation: cpuHistoryObservation(9), intervalAvailable: true, cpuCount: 2),
        CPUHistoryObservation(observation: cpuHistoryObservation(11),
            previousObservation: changed, intervalAvailable: true, cpuCount: 2)]

    try rejectsCPUHistory(cpuHistoryPayload(points: points), error: .interval, caseName: caseName)
}

private func rejects_a_changed_cpu_domain_for_an_available_interval() throws {
    // Goal: a CPU domain change requires an unavailable interval.
    // Method: keep consecutive observations while increasing their known CPU count.
    let changedDomain = [
        CPUHistoryObservation(observation: cpuHistoryObservation(10),
            previousObservation: cpuHistoryObservation(9), intervalAvailable: true, cpuCount: 1),
        CPUHistoryObservation(observation: cpuHistoryObservation(11),
            previousObservation: cpuHistoryObservation(10), intervalAvailable: true, cpuCount: 2)]

    try rejectsCPUHistory(cpuHistoryPayload(points: changedDomain), error: .interval,
        caseName: "known CPU domain changed")
}

private func rejects_a_value_in_an_unavailable_interval() throws {
    // Goal: an unavailable interval cannot carry a measured value.
    // Method: leave the interval unavailable while preserving its measured zero.
    let unavailable = [
        CPUHistoryObservation(observation: cpuHistoryObservation(10), previousObservation: nil,
            intervalAvailable: false, cpuCount: 2),
        CPUHistoryObservation(observation: cpuHistoryObservation(11),
            previousObservation: cpuHistoryObservation(10), intervalAvailable: true, cpuCount: 2)]

    try rejectsCPUHistory(cpuHistoryPayload(points: unavailable), error: .interval,
        caseName: "value in unavailable interval")
}

private func cpuHistoryDensePoints(count: Int) -> [CPUHistoryObservation] {
    precondition((1...4097).contains(count))

    return (0..<count).map { index in
        let publication = UInt64(index) + 10
        let previous = Observation(run: "run", boot: "boot", publication: publication - 1,
            runTick: publication - 1, wallNs: 181_800_000_000 + UInt64(index) * 200_000_000,
            windowLosses: nil)
        let current = Observation(run: "run", boot: "boot", publication: publication,
            runTick: publication, wallNs: previous.wallNs + 200_000_000, windowLosses: nil)

        return CPUHistoryObservation(observation: current, previousObservation: previous,
            intervalAvailable: true, cpuCount: 1024)
    }
}

private func cpuHistoryObservationVariants(_ original: Observation) -> [(String, Observation)] {
    [
        ("run", Observation(run: "other", boot: original.boot, publication: original.publication,
            runTick: original.runTick, wallNs: original.wallNs, windowLosses: nil)),
        ("boot", Observation(run: original.run, boot: "other", publication: original.publication,
            runTick: original.runTick, wallNs: original.wallNs, windowLosses: nil)),
        ("publication", Observation(run: original.run, boot: original.boot,
            publication: original.publication + 1, runTick: original.runTick,
            wallNs: original.wallNs, windowLosses: nil)),
        ("run tick", Observation(run: original.run, boot: original.boot,
            publication: original.publication, runTick: original.runTick + 1,
            wallNs: original.wallNs, windowLosses: nil)),
        ("wall time", Observation(run: original.run, boot: original.boot,
            publication: original.publication, runTick: original.runTick,
            wallNs: original.wallNs + 1, windowLosses: nil))]
}

private func preserves_independent_cpu_history_values_and_exact_context() throws {
    // Goal: the CPU page keeps independent measured zero, missing and busy intervals.
    // Method: validate two logical CPUs against two exact common chart observations.
    let selection = try cpuHistorySelection()
    let payload = cpuHistoryPayload()

    let history = try SelectedCPUHistory(payload: payload, selection: selection)

    precondition(history.observation.identity == selection.selected?.identity)
    precondition(history.observations.count == 2)
    precondition(history.cpus[0].busyPercent == [0, 50])
    precondition(history.cpus[1].busyPercent == [nil, 100])
    precondition(history.firstNs == 102_000_000_000)
    precondition(history.endNs == 1_002_000_000_000)
}

private func rejects_a_predecessor_that_changes_an_existing_context_observation() throws {
    // Goal: an interval cannot silently move the start of an already known observation.
    // Method: keep the predecessor identity but change its wall time by a quarter second.
    let original = cpuHistoryObservation(10)
    let changed = Observation(run: original.run, boot: original.boot,
        publication: original.publication, runTick: original.runTick,
        wallNs: original.wallNs + 250_000_000, windowLosses: nil)
    let payload = cpuHistoryPayload(points: [
        CPUHistoryObservation(observation: original, previousObservation: cpuHistoryObservation(9),
            intervalAvailable: true, cpuCount: 2),
        CPUHistoryObservation(observation: cpuHistoryObservation(11), previousObservation: changed,
            intervalAvailable: true, cpuCount: 2)])

    try rejectsCPUHistory(payload, error: .interval)
}

private func rejectsCPUHistory(_ payload: CPUHistoryPayload, error expected: CPUHistoryError,
                               selection: Selection? = nil, caseName: String = "") throws {
    let selection = try selection ?? cpuHistorySelection()

    do {
        let accepted = try SelectedCPUHistory(payload: payload, selection: selection)
        preconditionFailure("Accepted \(caseName) at publication \(accepted.observation.publication)")
    } catch {
        precondition(error as? CPUHistoryError == expected, "\(caseName): \(error)")
    }
}

private func cpuHistoryObservation(_ publication: UInt64) -> Observation {
    Observation(run: "run", boot: "boot", publication: publication, runTick: publication,
        wallNs: (990 + publication) * 1_000_000_000, windowLosses: nil)
}

private func cpuHistorySelection(points: [Observation]? = nil,
                                 selected: Observation? = nil) throws -> Selection {
    let observations = points ?? [cpuHistoryObservation(10), cpuHistoryObservation(11)]
    let current = selected ?? cpuHistoryObservation(11)
    let observation: (Observation) -> [String: Any] = { point in
        ["run": point.run, "boot": point.boot, "publication": point.publication,
         "run_tick": point.runTick, "wall_ns": point.wallNs]
    }
    let metrics = observations.map { point in
        observation(point).merging(["segment": UInt64(1), "cpu_percent": 50]) { _, new in new }
    }
    let value: [String: Any] = ["second": current.wallNs / 1_000_000_000,
        "selected": observation(current), "alternatives": [observation(current)],
        "summary": metrics.last ?? [:], "trends": metrics, "context_seconds": UInt64(900)]
    let data = try JSONSerialization.data(withJSONObject: ["value": value])

    return try decodeRecordingResponse(data)
}

private func cpuHistoryPayload(observation: Observation? = nil,
                               points: [CPUHistoryObservation]? = nil,
                               cpus: [LogicalCPUHistoryRow]? = nil,
                               offset: UInt32 = 0, total: UInt32 = 2,
                               firstNs: UInt64 = 102_000_000_000,
                               endNs: UInt64 = 1_002_000_000_000) -> CPUHistoryPayload {
    CPUHistoryPayload(observation: observation ?? cpuHistoryObservation(11), contextSeconds: 900,
        firstNs: firstNs, endNs: endNs, offset: offset, total: total, retentionLimited: false,
        observations: points ?? [
            CPUHistoryObservation(observation: cpuHistoryObservation(10),
                previousObservation: cpuHistoryObservation(9), intervalAvailable: true,
                cpuCount: total),
            CPUHistoryObservation(observation: cpuHistoryObservation(11),
                previousObservation: cpuHistoryObservation(10), intervalAvailable: true,
                cpuCount: total)],
        cpus: cpus ?? [LogicalCPUHistoryRow(cpu: 0, busyPercent: [0, 50]),
            LogicalCPUHistoryRow(cpu: 1, busyPercent: [nil, 100])])
}
