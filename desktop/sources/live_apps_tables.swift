import SwiftUI

struct LiveAppGroupTable: View {
    @Environment(\.flightdeckTheme) private var theme
    let groups: [LiveAppProcessGroup]
    let selected: Data?
    let onSelect: (Data?) -> Void

    var body: some View {
        Table(groups) {
            TableColumn("Process group") { group in
                Button(group.displayName) { onSelect(group.key) }.buttonStyle(.plain)
                    .foregroundStyle(selected == group.key ? theme.focus : theme.text)
                    .disabled(group.key == nil)
            }.width(min: 160, ideal: 230)
            TableColumn("Instances") { Text(String($0.instances)) }.width(min: 65, ideal: 75)
            TableColumn("CPU") { group in
                cell(dashboardReading(group.cpuPercentOneCore, unit: "% of one core"), group.cpuNs)
            }.width(min: 130, ideal: 180)
            TableColumn("Resident") { group in
                cell(liveAppMemory(group.residentBytes.knownValue), group.residentBytes)
            }.width(min: 130, ideal: 180)
            TableColumn("Footprint") { group in
                cell(liveAppMemory(group.footprintBytes.knownValue), group.footprintBytes)
            }.width(min: 130, ideal: 180)
            TableColumn("Power") { group in
                cell(dashboardReading(group.energyWatts, unit: "W"), group.energyNj)
            }.width(min: 130, ideal: 180)
            TableColumn("Energy") { group in
                cell(dashboardReading(group.energyJoules, unit: "J"), group.energyNj)
            }.width(min: 130, ideal: 180)
        }.font(theme.tableFont)
            .frame(height: min(theme.chartHeight, CGFloat(groups.count + 1) * theme.panelPadding * 2))
            .accessibilityLabel("Captured executable groups")
    }

    private func cell(_ reading: String, _ measure: LiveAppMeasure) -> some View {
        HStack(spacing: theme.fieldGap / 2) {
            Text(reading).monospacedDigit()
            if measure.unavailableMembers > 0 || measure.overflowed {
                Text("\(measure.measuredMembers)/\(UInt64(measure.measuredMembers) + UInt64(measure.unavailableMembers)) measured"
                    + (measure.overflowed ? " · overflow" : ""))
                    .font(.caption2).foregroundStyle(theme.muted)
            }
        }.help(measure.coverage)
    }
}

struct LiveAppInstances: View {
    @Environment(\.flightdeckTheme) private var theme
    let detail: LiveAppDetailPage
    let selectedGroup: Data?
    private var processes: [LiveAppProcess] {
        detail.processes.filter { selectedGroup == nil || $0.executableName?.bytes == selectedGroup }
    }

    var body: some View {
        Panel(title: "Process instances",
            explanation: "PID plus kernel creation identity identifies a process. Exec can change its program without starting a new process. Exact counters and refusal reasons remain separate from display units.") {
            if processes.isEmpty {
                Text("No members of this group are on the loaded page.").foregroundStyle(theme.muted)
            } else {
                table
            }
        }
    }

    private var table: some View {
        Table(processes) {
            TableColumn("PID") { Text(String($0.pid)) }.width(min: 60, ideal: 70)
            TableColumn("Creation ID") { process in
                Text(process.creation.map { String($0.uniqueId.value) } ?? "Unavailable")
            }.width(min: 165, ideal: 180)
            TableColumn("PID version") { process in
                Text(process.creation.map { String($0.pidVersion) } ?? "Unavailable")
            }.width(min: 75, ideal: 90)
            TableColumn("Executable") { process in
                Text(process.executableName?.display ?? "Unavailable")
                    .help(process.appAttribution.map { "App attribution: " + $0.rawValue } ?? "Captured executable")
            }.width(min: 150, ideal: 200)
            TableColumn("CPU") { process in
                amount(dashboardReading(process.cpuPercentOneCore, unit: "% of one core"), process.cpuNs)
            }.width(min: 140, ideal: 180)
            TableColumn("Resident") { process in
                amount(liveAppMemory(process.residentBytes.known?.value), process.residentBytes)
            }.width(min: 140, ideal: 180)
            TableColumn("Footprint") { process in
                amount(liveAppMemory(process.footprintBytes.known?.value), process.footprintBytes)
            }.width(min: 140, ideal: 180)
            TableColumn("Power") { process in
                amount(dashboardReading(process.energyWatts, unit: "W"), process.energyNj)
            }.width(min: 140, ideal: 180)
            TableColumn("Energy") { process in
                amount(dashboardReading(process.energyJoules, unit: "J"), process.energyNj)
            }.width(min: 140, ideal: 180)
        }.font(theme.tableFont)
            .frame(height: min(theme.chartHeight * 2,
                CGFloat(processes.count + 1) * theme.panelPadding * 2))
            .accessibilityLabel("Exact process creation identities and readings")
    }

    private func amount(_ reading: String, _ quantity: LiveAppAmount) -> some View {
        HStack(spacing: theme.fieldGap / 2) {
            Text(reading)
            if let reason = quantity.unavailable {
                Text(reason.rawValue.replacingOccurrences(of: "_", with: " "))
                    .font(.caption2).foregroundStyle(theme.muted)
            }
        }.monospacedDigit()
    }
}

struct LiveAppProcessRowIdentity: Hashable {
    let pid: Int32
    let uniqueId: UInt64?
}

extension LiveAppProcess: Identifiable {
    var id: LiveAppProcessRowIdentity {
        LiveAppProcessRowIdentity(pid: pid, uniqueId: creation?.uniqueId.value)
    }
}
