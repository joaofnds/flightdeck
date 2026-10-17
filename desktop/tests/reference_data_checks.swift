import Foundation

@MainActor
func runReferenceDataChecks() throws {
    // Decode the published capture and compare distinct source units and repeated/missing values.
    let directory = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
        .appendingPathComponent("desktop/resources", isDirectory: true)
    let snapshot = try ReferenceSnapshot.load(directory: directory)
    precondition(snapshot.series.count == 95)
    precondition(snapshot.series.values.reduce(0) { $0 + $1.count } == 1_156)
    precondition(snapshot.value("cpu0", at: 0) == 23.446)
    precondition(snapshot.value("cpu0", at: 119) == 15.361)
    precondition(snapshot.value("cpu0", at: 120) == 15.361)
    precondition(snapshot.value("thermal0", at: 7) == 1)
    precondition(snapshot.value("thermal0", at: 8) == 0)
    precondition(snapshot.series["appCpu"]?.first?.values[0] == nil)
    precondition(snapshot.value("unrecorded", at: 0) == nil)
    precondition(snapshot.value("cpu0", at: -1) == nil)
    precondition(snapshot.value("cpu0", at: 121) == nil)
    precondition(snapshot.end - snapshot.start == 3_600)
    precondition(snapshot.step == 30)
    try checkReferenceCatalog(snapshot)
    try checkReferenceDiagnostics(snapshot.selfCapture)
    try rejectDamagedReferenceResources(directory)
    runDashboardProjectionChecks(snapshot: snapshot)
    print("Reference capture, catalog, diagnostics and corruption checks passed")
}

private func checkReferenceCatalog(_ snapshot: ReferenceSnapshot) throws {
    // Match the original page forms and calculations rather than only a panel count.
    let ids = Set(snapshot.navigation.compactMap(\.first))
    precondition(ids == Set(["overview", "cpu", "apps", "gpu", "memory", "energy",
                             "storage", "network", "battery", "computer", "self"]))
    precondition(Set(snapshot.pages.keys) == ids)
    precondition(Set(snapshot.hourPages.keys) == ids)
    let panels = snapshot.pages["storage"] ?? []
    let latency = try requireReferencePanel(panels, id: "latency")
    precondition(latency.unit == "ms")
    precondition(latency.lines.first?.values.last == 0.18028)
    precondition(latency.lines.last?.values.last == 0.023856)
    let disk = try requireReferencePanel(panels, id: "disk")
    precondition(disk.lines.last?.negative == true)
    precondition((disk.lines.last?.values.last ?? nil).map { $0 >= 0 } == true)
    let overview = snapshot.pages["overview"] ?? []
    precondition(overview.first?.id == "overviewCpu")
    precondition(overview.last?.id == "hardware")
    precondition(overview.filter { $0.kind == "ranking" }.count == 6)
    precondition(overview.filter { $0.kind == "fold" }.count == 2)
    let cpu = snapshot.pages["cpu"] ?? []
    precondition(cpu.contains { $0.kind == "core-bars" && $0.lines.count == 18 })
    precondition(cpu.contains { $0.kind == "heatmap" && $0.lines.count == 18 })
    precondition(snapshot.appPages.count == 12)
    for panels in snapshot.appPages.values {
        precondition(panels.filter { $0.kind == "chart" }.count == 18)
    }
    precondition(snapshot.help["cpu"]?.contains { $0.contains("100%") } == true)
    precondition(snapshot.selfPanels.count == 6)
    precondition(snapshot.selfPanels.first?.times == [60, 120, 180, 240, 300])
}

private func checkReferenceDiagnostics(_ capture: ReferenceSelfCapture) throws {
    // Keep the unrelated saved run's elapsed clock and report denominators intact.
    precondition(capture.wallTime == nil)
    precondition(capture.duration == 300)
    precondition(capture.reports.count == 30)
    precondition(capture.minutes.count == 5)
    precondition(capture.seals.count == 6)
    precondition(capture.reports.first?.powerMw == 25.2)
    precondition(capture.tracking.missed == 8)
    precondition(capture.minutes.first?.elapsedNs == 60_000_258_541)
    precondition(capture.minutes.first?.combinedCpuNs == 818_667_457)
    precondition(capture.provenance.first?.sha256 ==
        "3610ba975606938cc170b6c2a9e0bfab053924be0a6e2020dc3af08588ba3776")
    precondition(capture.drain.status == 0)
}

private func rejectDamagedReferenceResources(_ source: URL) throws {
    // An incomplete or corrupt bundle must fail, never silently load another source.
    let temporary = FileManager.default.temporaryDirectory
        .appendingPathComponent("flightdeck-reference-" + UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: false)
    do {
        for name in ["reference_snapshot", "reference_catalog", "reference_self"] {
            try FileManager.default.copyItem(
                at: source.appendingPathComponent(name).appendingPathExtension("json"),
                to: temporary.appendingPathComponent(name).appendingPathExtension("json"))
        }
        let resource = temporary.appendingPathComponent("reference_snapshot.json")
        let original = try Data(contentsOf: resource)
        guard var capture = try JSONSerialization.jsonObject(with: original) as? [String: Any] else {
            throw ReferenceDataError.invalid("Test capture is not an object.")
        }
        capture["step"] = 0
        try JSONSerialization.data(withJSONObject: capture).write(to: resource)
        try requireReferenceFailure(directory: temporary)
        capture["step"] = 30
        guard var series = capture["series"] as? [String: [[String: Any]]],
              var rows = series["cpu0"], !rows.isEmpty else {
            throw ReferenceDataError.invalid("Test capture has no CPU series.")
        }
        rows[0]["values"] = [0]
        series["cpu0"] = rows
        capture["series"] = series
        try JSONSerialization.data(withJSONObject: capture).write(to: resource)
        try requireReferenceFailure(directory: temporary)
        try original.write(to: resource)
        try FileManager.default.removeItem(at: resource)
        try requireReferenceFailure(directory: temporary)
        try Data(repeating: 32, count: 8 * 1024 * 1024 + 1).write(to: resource)
        try requireReferenceFailure(directory: temporary)
    } catch {
        try FileManager.default.removeItem(at: temporary)
        throw error
    }
    try FileManager.default.removeItem(at: temporary)
}

private func requireReferenceFailure(directory: URL) throws {
    do {
        let unexpected = try ReferenceSnapshot.load(directory: directory)
        throw ReferenceDataError.invalid("Damaged resources were accepted: " + unexpected.source)
    } catch ReferenceDataError.invalid(let message) where message.hasPrefix("Damaged resources") {
        throw ReferenceDataError.invalid(message)
    } catch {
        // The loader's explicit error is the expected result.
    }
}

private func requireReferencePanel(_ panels: [ReferencePanel], id: String) throws -> ReferencePanel {
    guard let panel = panels.first(where: { $0.id == id }) else {
        throw ReferenceDataError.invalid("Missing reference panel: " + id)
    }
    return panel
}
