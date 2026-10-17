import SwiftUI

struct Console: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme

    var body: some View {
        NavigationSplitView {
            VStack(alignment: .leading, spacing: 6) {
                Label("FLIGHTDECK", systemImage: "waveform.path.ecg")
                    .font(theme.brandFont).tracking(1.4).padding(.bottom, 28)
                navigation("Overview", icon: "square.grid.2x2")
                navigation("CPU & processes", icon: "cpu")
                navigation("Recorded readings", icon: "chart.xyaxis.line")
                Spacer()
                Divider()
                Label("Historical recording", systemImage: "clock.arrow.circlepath")
                    .font(.caption).foregroundStyle(theme.muted).padding(.top, 10)
                Button("Open recording…", action: model.open).buttonStyle(.plain).padding(.vertical, 8)
                    .keyboardShortcut("o")
            }.padding(theme.panelPadding).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .background(theme.background)
                .navigationSplitViewColumnWidth(min: 180, ideal: 200, max: 240)
        } detail: {
            VStack(spacing: 0) {
                header
                Divider().overlay(theme.border)
                ScrollView {
                    VStack(alignment: .leading, spacing: theme.sectionGap) {
                        if let error = model.error {
                            ContentUnavailableView {
                                Label("Recording unavailable", systemImage: "externaldrive.badge.exclamationmark")
                            } description: { Text(error) }
                            actions: { Button("Choose a recording…", action: model.open) }
                        } else if model.catalog?.lastSecond == nil, !model.busy {
                            ContentUnavailableView("No saved timestamps", systemImage: "clock.badge.questionmark",
                                description: Text("Choose a recording with committed, timestamped ticks. Unknown-clock history cannot be assigned to a guessed second."))
                        } else {
                            if let losses = model.selection?.selected?.windowLosses, losses.hasLosses {
                                Label("This window reports \(losses.publications) lost publications and \(losses.events) lost events" + (losses.unknown ? "; additional loss is unknown." : "."), systemImage: "exclamationmark.triangle")
                                    .font(.callout).foregroundStyle(theme.warning)
                            }
                            if model.route == "Overview" { Dashboard() }
                            RecordedReadings()
                            if model.route == "CPU & processes" { MachineContext() }
                        }
                    }.padding(theme.contentPadding)
                }
                footer
            }.background(theme.background).foregroundStyle(theme.text)
        }
        .navigationTitle("Flightdeck")
        .tint(theme.focus)
        .preferredColorScheme(.dark)
        .onAppear { model.load() }
    }

    private func navigation(_ title: String, icon: String) -> some View {
        Button {
            if title == "CPU & processes" { model.enterProcesses() }
            else { model.route = title }
        } label: {
            Label(title, systemImage: icon).font(theme.navigationFont)
                .frame(maxWidth: .infinity, alignment: .leading).padding(11)
                .background(model.route == title ? theme.navigationAccent.opacity(0.14) : .clear,
                            in: RoundedRectangle(cornerRadius: theme.controlRadius))
                .foregroundStyle(model.route == title ? theme.navigationAccent : theme.text)
        }.buttonStyle(.plain)
    }

    private var header: some View {
        HStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 5) {
                Text(model.route).font(theme.titleFont)
                if let second = model.selection?.second {
                    Text(Date(timeIntervalSince1970: Double(second)), format: .dateTime
                        .year().month().day().hour().minute().second().timeZone())
                        .font(.callout).foregroundStyle(theme.muted)
                }
            }
            Spacer()
            if model.busy { ProgressView().controlSize(.small).accessibilityLabel("Reading recording") }
            Label("Last 15 minutes", systemImage: "clock").font(.callout).foregroundStyle(theme.muted)
            HStack {
                Button { step(-1) } label: { Image(systemName: "chevron.left") }.help("Previous second")
                TextField("Unix second", text: $model.secondText).frame(width: 110)
                    .onSubmit { if let value = UInt64(model.secondText) { model.load(second: value) } }
                    .accessibilityLabel("Recorded Unix second")
                Button { step(1) } label: { Image(systemName: "chevron.right") }.help("Next second")
            }
            Button { model.load(second: model.selection?.second, refresh: true) }
                label: { Image(systemName: "arrow.clockwise") }.help("Refresh committed history")
                .keyboardShortcut("r")
        }.padding(.horizontal, 24).padding(.vertical, 18)
    }

    private var footer: some View {
        HStack {
            Text(model.path).lineLimit(1).truncationMode(.middle).textSelection(.enabled)
            Spacer()
            if let catalog = model.catalog {
                Text("\(catalog.windows) sealed windows")
                if let first = catalog.firstSecond, let last = catalog.lastSecond {
                    Text("\(first.formatted(.number.grouping(.never)))…\(last.formatted(.number.grouping(.never))) UTC seconds")
                }
                if catalog.unknownClockTicks > 0 { Text("\(catalog.unknownClockTicks) ticks without wall time") }
            }
        }.font(.caption).foregroundStyle(theme.muted).padding(12)
            .background(theme.panel)
    }

    private func step(_ delta: Int64) {
        guard let second = model.selection?.second ?? UInt64(model.secondText) else { return }
        if delta < 0, second > 0 { model.load(second: second - 1) }
        if delta > 0, second < UInt64.max { model.load(second: second + 1) }
    }
}

struct RecordedReadings: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @State private var expanded = true

    var body: some View {
        DisclosureGroup(model.route == "CPU & processes" ? "Processes" : "Recorded readings",
                        isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: theme.panelPadding) {
                HStack {
                    if model.route == "CPU & processes" {
                        Text("Processes at the selected publication").foregroundStyle(theme.muted)
                    } else {
                        Picker("Source", selection: Binding(get: { model.table }, set: model.chooseTable)) {
                            ForEach(Array((model.catalog?.tables ?? []).enumerated()), id: \.offset) { index, table in
                                Text(table.replacingOccurrences(of: "_", with: " ").capitalized).tag(UInt32(index))
                            }
                        }.frame(maxWidth: 260)
                    }
                    if let alternatives = model.selection?.alternatives, alternatives.count > 1 {
                        Menu("\(alternatives.count) observations in this second") {
                            ForEach(Array(alternatives.enumerated()), id: \.offset) { index, observation in
                                Button("Run \(observation.run.prefix(8)) · publication \(observation.publication)") {
                                    model.load(second: model.selection?.second, alternative: UInt32(index))
                                }
                            }
                        }
                    }
                    Spacer()
                    if let page = model.page {
                        Text("\(page.offset + UInt64(page.rows.count)) of \(page.total) rows").foregroundStyle(theme.muted)
                        Button("Previous") { pageBy(-512) }.disabled(page.offset == 0)
                        Button("Next") { pageBy(512) }.disabled(page.offset + UInt64(page.rows.count) >= page.total)
                    }
                }
                if let page = model.page {
                    Text(description(page.name)).font(.callout).foregroundStyle(theme.muted)
                    RecordedTable(page: page, fields: fields(page), selection: $model.row)
                    if let selected = model.row, selected < page.rows.count {
                        Panel(title: "Recorded fields", explanation: "Exact stored values, in the unit named by each field. Unavailable represents SQL NULL; cumulative counters are not rates.") {
                            RecordedFields(page: page, row: page.rows[selected])
                        }
                    } else {
                        Text("Select a row to inspect every recorded field.").foregroundStyle(theme.muted)
                    }
                } else {
                    Text(model.busy ? "Reading selected tick…" : "No observation selected.")
                        .foregroundStyle(theme.muted).padding(.vertical, 30)
                }
            }.padding(.top, 18)
        }.font(.headline)
    }

    private func fields(_ page: TablePage) -> [String] {
        if page.name == "processes" { return ["pid", "awake_ns_at_start", "user_ns", "resident_bytes"] }
        if page.name == "events" { return ["event", "pid", "kind", "availability"] }
        return Array(page.columns.prefix(4))
    }

    private func description(_ name: String) -> String {
        if name == "processes" { return "Sampled processes · CPU time is cumulative nanoseconds. Names were not recorded. PID plus awake start time identifies a creation." }
        if name == "events" { return "Events received with this tick · kind: 1 start, 2 exec, 3 exit. Availability: 0 measured, 1 refused, 2 gone, 3 not applicable. These are not exact occurrence timestamps." }
        if name == "coalitions" { return "Raw resource coalitions. These are not app totals." }
        return "Exact source readings for the selected publication. Select a row for all recorded fields and units."
    }

    private func pageBy(_ delta: Int64) {
        guard let page = model.page, let selection = model.selection else { return }
        let offset = delta < 0 ? page.offset - 512 : page.offset + 512
        model.load(second: selection.second, offset: offset)
    }
}

struct MachineContext: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @State private var expanded = true

    var body: some View {
        DisclosureGroup("Machine context", isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: theme.panelPadding) {
                Text("All fields use the same saved publication as the process list. CPU ticks, traffic and paging counters are cumulative, not rates.")
                    .font(.callout).foregroundStyle(theme.muted)
                ForEach(MachineSource.allCases, id: \.self) { source in
                    if let page = model.machinePages[source] {
                        MachineReadings(source: source, page: page)
                    }
                }
                if model.selection?.selected == nil {
                    Text(model.busy ? "Reading selected tick…" : "No machine readings at this second.")
                        .foregroundStyle(theme.muted)
                }
            }.padding(.top, 18)
        }.font(.headline)
    }
}

private struct MachineReadings: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @State private var expanded = true
    let source: MachineSource
    let page: TablePage

    var body: some View {
        DisclosureGroup(source.title, isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: theme.panelGap) {
                if page.rows.isEmpty {
                    Text("No rows were recorded for this publication.").foregroundStyle(theme.muted)
                } else if page.rows.count == 1, source == .ticks || source == .memory {
                    RecordedFields(page: page, row: page.rows[0])
                } else {
                    RecordedTable(page: page, fields: page.columns, selection: .constant(nil))
                }
                HStack {
                    Text("\(page.offset + UInt64(page.rows.count)) of \(page.total) rows")
                        .foregroundStyle(theme.muted)
                    Spacer()
                    Button("Previous") { model.pageMachine(source, offset: page.offset - 512) }
                        .disabled(page.offset == 0 || model.busy)
                    Button("Next") { model.pageMachine(source, offset: page.offset + 512) }
                        .disabled(page.offset + UInt64(page.rows.count) >= page.total || model.busy)
                }.font(.callout)
            }.padding(.top, 16)
        }
    }
}

private struct RecordedFields: View {
    @Environment(\.flightdeckTheme) private var theme
    let page: TablePage
    let row: [String?]

    var body: some View {
        LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())],
                  alignment: .leading, spacing: theme.fieldGap) {
            ForEach(Array(page.columns.enumerated()), id: \.offset) { index, column in
                HStack {
                    Text(column).foregroundStyle(theme.muted)
                    Spacer()
                    Text(row[index] ?? "Unavailable").monospacedDigit().textSelection(.enabled)
                }.font(theme.tableFont)
            }
        }
    }
}
