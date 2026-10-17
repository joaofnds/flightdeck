import SwiftUI

struct LiveAppGroupTable: View {
    @Environment(\.flightdeckTheme) private var theme
    let groups: [LiveAppProcessGroup]
    let selected: Data?
    let onSelect: (Data?) -> Void

    var body: some View {
        LiveAppsNativeTable(data: .groups(groups, selected: selected), onSelect: onSelect)
            .frame(height: min(theme.chartHeight, CGFloat(groups.count + 1) * theme.panelPadding * 2))
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
                LiveAppsNativeTable(data: .processes(processes), onSelect: nil)
                    .frame(height: min(theme.chartHeight * 2,
                        CGFloat(processes.count + 1) * theme.panelPadding * 2))
            }
        }
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
