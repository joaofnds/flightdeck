import SwiftUI

struct LiveAppsPane: View {
    @Environment(\.flightdeckTheme) private var theme
    let list: LiveAppListPage?
    let detail: LiveAppDetailPage?
    let history: LiveAppHistoryView?
    let selectedKey: LiveAppKeyIdentity?
    let selectedProcessGroup: Data?
    let busy: Bool
    let error: String?
    let onSelectApp: (LiveAppKeyIdentity) -> Void
    let onSelectProcessGroup: (Data?) -> Void
    let onAppsPage: (UInt32) -> Void
    let onMembersPage: (UInt32) -> Void
    @State private var instancesExpanded = true
    @State private var historiesExpanded = true

    private var currentDetail: LiveAppDetailPage? {
        guard let list, let detail, detail.header.selected == list.header.selected,
              detail.app.key.identity == selectedKey else { return nil }

        return detail
    }
    private var appChoices: [LiveAppRow] {
        var choices = list?.apps ?? []
        if let selected = currentDetail?.app,
           !choices.contains(where: { $0.key.identity == selected.key.identity }) {
            choices.append(selected)
        }
        return choices.sorted { $0.displayName.localizedStandardCompare($1.displayName) == .orderedAscending }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            if let error { Label(error, systemImage: "exclamationmark.circle").foregroundStyle(theme.warning) }
            selectors
            if let detail = currentDetail {
                selectedSummary(detail)
                processGroups(detail)
                DisclosureGroup("Process instances", isExpanded: $instancesExpanded) {
                    LiveAppInstances(detail: detail, selectedGroup: selectedProcessGroup)
                        .padding(.top, theme.panelGap)
                }.font(.headline)
                Text("App network, GPU, disk I/O, open files and sockets are not collected in this view yet.")
                    .font(.caption).foregroundStyle(theme.muted)
            } else {
                emptyState
            }
            if selectedKey != nil { histories }
        }
    }

    private var selectors: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            HStack(spacing: theme.panelGap) {
                Picker("App", selection: Binding(get: { selectedKey }, set: { key in
                    if let key { onSelectApp(key) }
                })) {
                    Text("Select an App").tag(Optional<LiveAppKeyIdentity>.none)
                    ForEach(appChoices, id: \.key.identity) { app in
                        Text(app.displayName).tag(Optional(app.key.identity))
                    }
                    if let selectedKey, !appChoices.contains(where: { $0.key.identity == selectedKey }) {
                        Text("Selected App is not on this page").tag(Optional(selectedKey))
                    }
                }.frame(maxWidth: 360)
                if let detail = currentDetail { groupPicker(LiveAppGroups(detail: detail)) }
                if busy { ProgressView().controlSize(.small).accessibilityLabel("Reading live App metrics") }
                Spacer(minLength: 0)
            }
            if let list {
                LiveAppPager(title: "Apps", page: list.page, busy: busy, action: onAppsPage)
                if list.header.ungroupedMembers > 0 {
                    Text("\(list.header.ungroupedMembers) processes have unavailable App identity.")
                        .font(.caption).foregroundStyle(theme.muted)
                }
            }
        }
    }

    private func groupPicker(_ groups: LiveAppGroups) -> some View {
        Picker("Process group", selection: Binding(get: { selectedProcessGroup },
            set: { group in onSelectProcessGroup(group) })) {
            Text("All process groups").tag(Optional<Data>.none)
            ForEach(groups.rows) { group in
                if let key = group.key { Text(group.displayName).tag(Optional(key)) }
            }
            if let selectedProcessGroup,
               !groups.rows.contains(where: { $0.key == selectedProcessGroup }) {
                Text("Selected group is not on this page").tag(Optional(selectedProcessGroup))
            }
        }.frame(maxWidth: 360)
    }

    private func selectedSummary(_ detail: LiveAppDetailPage) -> some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            Text(detail.app.displayName).font(theme.titleFont)
            Text(detail.app.key.display).font(.caption).foregroundStyle(theme.muted).textSelection(.enabled)
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)],
                alignment: .leading, spacing: theme.panelGap) {
                LiveAppSummaryTile(title: "CPU", reading: dashboardReading(detail.app.cpuPercentOneCore,
                    unit: "% of one core"), note: "100% = one core", measure: detail.app.cpuNs)
                LiveAppSummaryTile(title: "Resident", reading: liveAppMemory(detail.app.residentBytes.knownValue),
                    note: "Shared pages count per process", measure: detail.app.residentBytes)
                LiveAppSummaryTile(title: "Footprint", reading: liveAppMemory(detail.app.footprintBytes.knownValue),
                    note: "Separate process memory gauge", measure: detail.app.footprintBytes)
                LiveAppSummaryTile(title: "Average power", reading: dashboardReading(detail.app.energyWatts, unit: "W"),
                    note: dashboardReading(detail.app.energyJoules, unit: "J") + " this interval",
                    measure: detail.app.energyNj)
            }
            Text(detail.app.membershipComplete ? "Known subtotals · observed membership complete"
                : "Known subtotals · membership uncertain")
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func processGroups(_ detail: LiveAppDetailPage) -> some View {
        let groups = LiveAppGroups(detail: detail)

        return Panel(title: "Inside \(detail.app.displayName)",
            explanation: "Grouped by captured executable bytes. These group subtotals cover the loaded process page. CPU and energy crossing an exec boundary are unavailable for grouping.") {
            Text(groups.coverage).font(.caption).foregroundStyle(theme.muted)
            LiveAppGroupTable(groups: groups.rows, selected: selectedProcessGroup,
                onSelect: onSelectProcessGroup)
            LiveAppPager(title: "Processes", page: detail.page, busy: busy, action: onMembersPage)
        }
    }

    private var histories: some View {
        DisclosureGroup("Selected App totals · last 15 minutes", isExpanded: $historiesExpanded) {
            if let history, history.key == selectedKey {
                LiveAppHistoryCharts(history: history).padding(.top, theme.panelGap)
            } else {
                Text("Waiting for live App history. Earlier readings will appear as they are observed.")
                    .font(.callout).foregroundStyle(theme.muted).padding(.top, theme.fieldGap)
            }
        }.font(.headline)
    }

    @ViewBuilder private var emptyState: some View {
        if busy {
            Text("Reading live App metrics…").foregroundStyle(theme.muted)
        } else if let list, list.header.appCount == 0 {
            Text("App identity is unavailable for the observed processes. Raw process readings remain available in CPU & processes.")
                .foregroundStyle(theme.muted)
        } else {
            Text("Select an App to see its live CPU, memory, energy and processes.")
                .foregroundStyle(theme.muted)
        }
    }
}

struct LiveAppHistoryView {
    let key: LiveAppKeyIdentity
    let cpu: TimeSeriesModel?
    let resident: TimeSeriesModel?
    let footprint: TimeSeriesModel?
    let power: TimeSeriesModel?
    let selectedTime: Double?
    let selectedID: String?
    let labels: [String]
    let coverage: String

    func selectedValue(model: TimeSeriesModel, line: TimeSeriesLine) -> Double? {
        precondition(line.values.count == model.samples.count)
        if let selectedID,
           let index = model.samples.firstIndex(where: { $0.id == selectedID }) {
            return line.values[index]
        }
        return nil
    }
}

private struct LiveAppSummaryTile: View {
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    let reading: String
    let note: String
    let measure: LiveAppMeasure

    var body: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            Text(title).font(.caption).foregroundStyle(theme.muted)
            Text(reading).font(theme.metricFont).monospacedDigit().lineLimit(1).minimumScaleFactor(0.7)
            Text(note).font(.caption).foregroundStyle(theme.muted)
            Text(measure.coverage).font(.caption2).foregroundStyle(theme.muted)
        }.frame(maxWidth: .infinity, alignment: .leading).padding(theme.panelPadding)
            .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
            .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
    }
}

private struct LiveAppPager: View {
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    let page: LiveAppPage
    let busy: Bool
    let action: (UInt32) -> Void

    var body: some View {
        HStack(spacing: theme.fieldGap) {
            Text("\(title) · \(page.total == 0 ? 0 : page.offset + 1)–\(min(page.total, page.offset + page.limit)) of \(page.total)")
                .font(.caption).foregroundStyle(theme.muted).monospacedDigit()
            if page.offset > 0 {
                Button("Previous") { action(page.offset >= page.limit ? page.offset - page.limit : 0) }
                    .disabled(busy).accessibilityLabel("Previous \(title.lowercased()) page")
            }
            if let next = page.nextOffset {
                Button("Next") { action(next) }.disabled(busy)
                    .accessibilityLabel("Next \(title.lowercased()) page")
            }
            Spacer(minLength: 0)
        }
    }
}

func liveAppMemory(_ value: UInt64?) -> String {
    dashboardReading(value.map { Double($0) / 1_073_741_824 }, unit: "GiB")
}
