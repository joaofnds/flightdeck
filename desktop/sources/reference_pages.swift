import SwiftUI

@MainActor
func referenceAppHistory(snapshot: ReferenceSnapshot, key: String, range: ClosedRange<Int>,
                         references: [ReferenceLine]) -> [ReferenceLine] {
    guard let metric = ReferenceAttribution.metrics.first(where: { $0.key == key }),
          let total = snapshot.totals[key] else { return [] }

    let ranked = (snapshot.series[key] ?? []).map { row in
        let samples = range.compactMap { row.values[$0] }
        let mean = samples.isEmpty ? 0 : samples.reduce(0, +) / Double(samples.count)
        return (row: row, mean: mean)
    }
    let rows = ranked.sorted { $0.mean > $1.mean }.prefix(8).map(\.row)
    let colors = ["#f2cc0c", "#5794f2", "#ff9830", "#f2495c", "#b877d9", "#37872d", "#e0b400", "#1f60c4"]
    let other = total.enumerated().map { index, value in
        value.map { max(0, $0 - rows.reduce(0) { $0 + ($1.values[index] ?? 0) }) * metric.scale }
    }
    var lines = [ReferenceLine(name: "Other", color: "#808080", values: other, fill: 0.45)]
    lines += rows.enumerated().map { index, row in
        ReferenceLine(name: row.app ?? "Unavailable", color: colors[index],
            values: row.values.map { $0.map { $0 * metric.scale } }, fill: 0.45)
    }
    return lines + references
}

struct ReferenceProcessCells {
    let values: [String]

    init(snapshot: ReferenceSnapshot?, app: String, process: String, index: Int) {
        let columns: [(String, Double, Int)] = [
            ("proc0", 1, 0), ("proc1", 1, 1), ("proc2", 1 / 1_073_741_824, 2),
            ("proc3", 1 / 1_073_741_824, 2), ("proc4", 1e-6, 2), ("proc5", 1e-6, 2),
            ("proc6", 1, 2), ("proc7", 1, 2), ("proc8", 1, 1), ("proc9", 1, 0),
            ("proc10", 1, 0), ("proc11", 1, 0)
        ]
        values = columns.map { key, scale, digits in
            let samples = snapshot?.series[key]?.first {
                $0.app == app && $0.labels["process"] == process
            }?.values
            let raw = samples.flatMap { $0.indices.contains(index) ? $0[index] : nil }
            return dashboardReading(raw.map { $0 * scale }, unit: "", digits: digits)
        }
    }
}

struct ReferencePages: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme

    var body: some View {
        LazyVStack(alignment: .leading, spacing: theme.sectionGap) {
            if model.route == "self" {
                ReferenceSelfMonitoring()
            } else if model.route == "computer" {
                computer
            } else {
                summary
                if model.route == "apps" { appPicker; processTable }
                ForEach(groups) { group in
                    ReferenceSection(group: group)
                }
            }
        }
    }

    private var panels: [ReferencePanel] {
        guard let snapshot = model.snapshot else { return [] }

        if model.route == "apps" {
            let template = snapshot.appPages[model.app] ?? []
            return template.filter { $0.kind != "process-table" }.map { panel in
                guard model.process != "*", panel.id.hasPrefix("process") else { return panel }

                var result = panel
                result.lines = panel.lines.filter { line in
                    line.name == model.process || [" received", " sent", " read", " written"].contains { line.name == model.process + $0 }
                }
                return result
            }
        }
        return (snapshot.pages[model.route] ?? []).map { panel in
            guard panel.kind == "chart", let key = panel.help,
                  ReferenceAttribution.metrics.contains(where: { $0.key == key }) else { return panel }

            var result = panel
            result.lines = referenceAppHistory(snapshot: snapshot, key: key,
                range: model.window.lower...model.window.upper, references: panel.lines.filter { $0.stack == false })
            return result
        }
    }

    private var groups: [ReferenceSectionGroup] {
        var result: [ReferenceSectionGroup] = []
        var current = ReferenceSectionGroup(id: "initial", title: nil, fold: false, panels: [])
        for panel in panels {
            if panel.kind == "section" || panel.kind == "fold" {
                if !current.panels.isEmpty { result.append(current) }
                current = ReferenceSectionGroup(id: panel.id, title: panel.title, fold: panel.kind == "fold", panels: [])
            } else {
                current.panels.append(panel)
            }
        }
        if !current.panels.isEmpty { result.append(current) }
        return result
    }

    private var appPicker: some View {
        HStack(spacing: theme.panelGap) {
            Picker("App", selection: Binding(get: { model.app }, set: model.inspect)) {
                ForEach(model.snapshot?.apps ?? [], id: \.self) { Text($0) }
            }.frame(maxWidth: 360)
            Picker("Process group", selection: $model.process) {
                Text("All process groups").tag("*")
                ForEach(processes, id: \.labels) { row in
                    Text(row.labels["process"] ?? "Unnamed").tag(row.labels["process"] ?? "")
                }
            }.frame(maxWidth: 360)
        }
    }

    private var processes: [ReferenceSeries] {
        (model.snapshot?.series["proc0"] ?? []).filter { $0.app == model.app }
    }

    private var processTable: some View {
        Panel(title: "Inside \(model.app)", explanation: "Captured groups by reported process name, not PID or unique process creation. Twelve captured apps have process histories; other apps may only have totals.") {
            if processes.isEmpty {
                Text("App history retained; process groups were not captured for this app.").foregroundStyle(theme.muted)
                selectedTotals
            } else {
                ScrollView(.horizontal) {
                    Grid(alignment: .leading, horizontalSpacing: 24, verticalSpacing: 10) {
                        GridRow {
                            ForEach(["Process group", "Instances", "CPU %", "Resident GiB", "Footprint GiB", "Network MB/s", "Disk MB/s", "GPU %", "Power W", "Wakeups/s", "Threads", "Files", "Sockets"], id: \.self) { Text($0).font(.caption).foregroundStyle(theme.muted) }
                        }
                        ForEach(processes.sorted { (processValue("proc1", name: $0.labels["process"] ?? "") ?? 0) > (processValue("proc1", name: $1.labels["process"] ?? "") ?? 0) }, id: \.labels) { row in
                            let name = row.labels["process"] ?? "Unnamed"
                            GridRow {
                                Button(name) { model.process = name }.buttonStyle(.plain)
                                    .foregroundStyle(model.process == name ? theme.focus : theme.text)
                                ForEach(Array(ReferenceProcessCells(snapshot: model.snapshot, app: model.app,
                                    process: name, index: model.window.selected).values.enumerated()), id: \.offset) { _, cell in
                                    Text(cell).monospacedDigit()
                                }
                            }.font(theme.tableFont)
                        }
                    }.padding(.vertical, 8).textSelection(.enabled)
                }
            }
        }
    }

    private func processValue(_ key: String, name: String) -> Double? {
        model.snapshot?.series[key]?.first { $0.app == model.app && $0.labels["process"] == name }?.values[model.window.selected]
    }

    private var selectedTotals: some View {
        LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: theme.panelGap) {
            ForEach(Array(ReferenceAttribution.metrics.enumerated()), id: \.offset) { _, metric in
                let row = model.snapshot?.series[metric.key]?.first { $0.app == model.app }
                ReferenceChart(panel: ReferencePanel(id: "selected" + metric.key, title: metric.title + " · " + model.app,
                    unit: metric.unit, lines: [ReferenceLine(name: model.app, color: metric.color, values: row?.values.map { $0.map { $0 * metric.scale } } ?? Array(repeating: nil, count: 121))], help: metric.key))
            }
        }
    }

    private var summary: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], spacing: theme.fieldGap) {
            ForEach(Array(summaryItems.enumerated()), id: \.offset) { _, item in
                Button { model.route = item.route } label: {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(item.title).font(.caption).foregroundStyle(theme.muted)
                        Text(item.reading).font(theme.metricFont).monospacedDigit().lineLimit(1).minimumScaleFactor(0.65)
                        Text(item.note).font(.caption2).foregroundStyle(theme.muted)
                    }.frame(maxWidth: .infinity, alignment: .leading).padding(theme.panelPadding)
                        .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
                        .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
                }.buttonStyle(.plain)
            }
        }
    }

    private struct SummaryItem {
        let title: String
        let reading: String
        let note: String
        let route: String
    }

    private func item(_ title: String, _ key: String, _ unit: String, _ route: String, _ note: String, scale: Double = 1) -> SummaryItem {
        SummaryItem(title: title, reading: dashboardReading(model.value(key).map { $0 * scale }, unit: unit), note: note, route: route)
    }

    private var summaryItems: [SummaryItem] {
        let cpu = item("CPU", "cpu0", "%", "cpu", "All 18 cores")
        let gpu = item("GPU", "gpu0", "%", "gpu", "Active time")
        let power = item("System power", "power0", "W", "energy", "Whole-machine sensor")
        let receive = item("Receive", "network0", "MB/s", "network", "External interfaces", scale: 1e-6)
        let read = item("Disk read", "disk0", "MB/s", "storage", "Machine device I/O", scale: 1e-6)
        switch model.route {
        case "cpu": return [cpu, item("Performance", "cpuUsage1", "%", "cpu", "12 cores · cluster capacity"), item("Super", "cpuUsage2", "%", "cpu", "6 cores · cluster capacity"), item("Load", "load0", "work items", "cpu", "1 minute · running or waiting")]
        case "gpu": return [gpu, item("Active frequency", "gpuDetail1", "GHz", "gpu", "Cycles / active time", scale: 0.001), item("Memory in use", "gpuMemory0", "GiB", "gpu", "Unified-memory allocation", scale: 1 / 1_073_741_824), item("GPU power", "gpuPower1", "W", "gpu", "Recorded energy / second")]
        case "memory": return [SummaryItem(title: "Pressure", reading: state("pressure0", states: ["Normal", "Warning", "Critical"]), note: "macOS verdict", route: "memory"), item("Memory in use", "memoryUsed0", "GiB", "memory", "Of 48 GiB installed", scale: 1 / 1_073_741_824), item("Swap used", "swapDetail0", "GiB", "memory", "On the startup disk", scale: 1 / 1_073_741_824)]
        case "energy": return [power, item("GPU power", "gpuPower1", "W", "gpu", "Component energy / second"), SummaryItem(title: "Thermal state", reading: state("thermal0", states: ["Nominal", "Fair", "Serious", "Critical"]), note: "macOS verdict", route: "energy")]
        case "storage": return [read, item("Disk write", "disk1", "MB/s", "storage", "Machine device I/O", scale: 1e-6), item("Read operations", "diskOps0", "ops/s", "storage", "Completed requests"), item("Write operations", "diskOps1", "ops/s", "storage", "Completed requests")]
        case "network": return [receive, item("Send", "network1", "MB/s", "network", "External interfaces", scale: 1e-6), item("Received packets", "packets0", "pps", "network", "Packets / second"), item("Sent packets", "packets1", "pps", "network", "Packets / second")]
        case "battery": return [item("Charge", "battery0", "%", "battery", "Reported full capacity"), SummaryItem(title: "Charging", reading: model.value("battery1").map { $0 == 100 ? "Yes" : $0 == 0 ? "No" : "Unavailable" } ?? "Unavailable", note: "Battery-controller state", route: "battery"), item("Battery flow", "batteryPower0", "W", "battery", "Positive in · negative out"), item("Health", "batteryHealth0", "%", "battery", "Full capacity / design capacity")]
        case "apps": return ReferenceAttribution.metrics.map { metric in
            let value = model.snapshot?.series[metric.key]?.first { $0.app == model.app }?.values[model.window.selected]
            return SummaryItem(title: metric.title, reading: dashboardReading(value.map { $0 * metric.scale }, unit: metric.unit), note: metric.unit + " · " + model.app, route: "apps")
        }
        default: return [cpu, gpu, SummaryItem(title: "Memory pressure", reading: state("pressure0", states: ["Normal", "Warning", "Critical"]), note: "macOS verdict", route: "memory"), power, read, receive]
        }
    }

    private func state(_ key: String, states: [String]) -> String {
        guard let value = model.value(key), value >= 0, value < Double(states.count) else { return "Unavailable" }

        return states[Int(value)]
    }

    private var computer: some View {
        LazyVStack(alignment: .leading, spacing: theme.sectionGap) {
            Panel(title: "MacBook Pro", explanation: "Hardware context from the captured prototype, not current-machine detection.") {
                Text("16-inch · 2026 · captured Mac17,8 reference").foregroundStyle(theme.muted)
                ForEach(Array((model.snapshot?.computer ?? []).enumerated()), id: \.offset) { _, spec in
                    HStack { Text(spec.name).foregroundStyle(theme.muted); Spacer(); Text(spec.value).textSelection(.enabled) }
                }
            }
            ReferenceComputerSources()
        }
    }
}

struct ReferenceSectionGroup: Identifiable {
    let id: String
    let title: String?
    let fold: Bool
    var panels: [ReferencePanel]
}

private struct ReferenceSection: View {
    @Environment(\.flightdeckTheme) private var theme
    @State private var expanded = true
    let group: ReferenceSectionGroup

    var body: some View {
        if group.fold {
            DisclosureGroup(group.title ?? "Details", isExpanded: $expanded) {
                ReferencePanelGrid(panels: group.panels).padding(.top, theme.panelGap)
            }.font(.headline)
        } else {
            VStack(alignment: .leading, spacing: theme.panelGap) {
                if let title = group.title { Text(title).font(.title3.weight(.semibold)) }
                ReferencePanelGrid(panels: group.panels)
            }
        }
    }
}

private struct ReferencePanelGrid: View {
    @Environment(\.flightdeckTheme) private var theme
    let panels: [ReferencePanel]

    private var rows: [[ReferencePanel]] {
        var rows: [[ReferencePanel]] = [], pending: [ReferencePanel] = []
        for panel in panels {
            if ["hardware", "heatmap", "process-table"].contains(panel.kind ?? "") {
                if !pending.isEmpty { rows.append(pending); pending = [] }
                rows.append([panel])
            } else {
                pending.append(panel)
                if pending.count == 2 { rows.append(pending); pending = [] }
            }
        }
        if !pending.isEmpty { rows.append(pending) }
        return rows
    }

    var body: some View {
        VStack(spacing: theme.panelGap) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                if row.count == 1 {
                    ReferencePanelView(panel: row[0])
                } else {
                    ViewThatFits(in: .horizontal) {
                        HStack(alignment: .top, spacing: theme.panelGap) {
                            ForEach(row) { panel in ReferencePanelView(panel: panel).frame(minWidth: 330) }
                        }
                        VStack(spacing: theme.panelGap) {
                            ForEach(row) { panel in ReferencePanelView(panel: panel) }
                        }
                    }
                }
            }
        }
    }
}

private struct ReferencePanelView: View {
    @EnvironmentObject private var model: ReferenceModel
    let panel: ReferencePanel

    var body: some View {
        switch panel.kind {
        case "ranking": ReferenceAttribution(panel: panel)
        case "core-bars", "heatmap": ReferenceCores(panel: panel)
        case "pressure": ReferenceUtilization()
        case "timeline": ReferenceTimeline(panel: panel)
        case "quantities": ReferenceQuantities(panel: panel)
        case "hardware": MacBookSchematic(readings: schematicReadings, preview: model.schematicPreview)
        default: ReferenceChart(panel: panel)
        }
    }

    private var schematicReadings: SchematicReadings {
        let index = model.window.selected
        func fan(_ channel: String) -> Double? {
            model.snapshot?.series["fans0"]?.first { $0.labels["fan"] == channel }?.values[index]
        }
        let thermal = ["Nominal", "Fair", "Serious", "Critical"]
        let value = model.value("thermal0")
        return SchematicReadings(cpuTemperature: model.value("temperatures0"), gpuTemperature: model.value("temperatures1"),
            fan0: fan("0"), fan1: fan("1"), cpuPercent: model.value("cpu0"), gpuPercent: model.value("gpu0"),
            memoryUsedGiB: model.value("memoryUsed0").map { $0 / 1_073_741_824 },
            memoryTotalGiB: model.value("installed").map { $0 / 1_073_741_824 }, memoryTemperature: model.value("temperatures3"),
            batteryPercent: model.value("battery0"), batteryWatts: model.value("batteryPower0"),
            thermalState: value.flatMap { $0 >= 0 && $0 < 4 ? thermal[Int($0)] : nil })
    }
}
