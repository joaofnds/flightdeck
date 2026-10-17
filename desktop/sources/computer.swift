import SwiftUI

struct ComputerPane: View {
    @EnvironmentObject private var computer: ComputerInventoryModel
    @Environment(\.flightdeckTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            Text("Information about the Mac running Flightdeck, read on demand. This is independent of the selected metric time or recording.")
                .font(.callout).foregroundStyle(theme.muted)
            if let snapshot = computer.snapshot {
                Panel(title: "Hardware", explanation: "Reported by this Mac's kernel. CPU counts are the maximum available this boot.") {
                    row("Model identifier", reading: snapshot.modelIdentifier)
                    row("Chip", reading: snapshot.chipName)
                    row("Architecture", reading: snapshot.architecture)
                    row("Physical CPU cores · maximum this boot", reading: snapshot.physicalCores) {
                        $0.formatted()
                    }
                    row("Logical CPUs · maximum this boot", reading: snapshot.logicalCores) {
                        $0.formatted()
                    }
                    row("Installed memory", reading: snapshot.memoryBytes) {
                        dashboardReading(Double($0) / 1_073_741_824, unit: "GiB")
                    }
                }
                Panel(title: "System", explanation: "Current viewer-host software and boot information, not hardware metadata from a saved recording.") {
                    row("macOS version", reading: snapshot.macOSVersion)
                    row("macOS build", reading: snapshot.macOSBuild)
                    row("Darwin kernel", reading: snapshot.kernelRelease)
                    row("VM page size", reading: snapshot.pageBytes) { $0.formatted() + " bytes" }
                    row("Boot time", reading: snapshot.bootTime) {
                        $0.formatted(date: .abbreviated, time: .standard)
                    }
                }
            } else {
                ProgressView("Reading computer information…")
            }
        }.onAppear(perform: computer.loadIfNeeded)
    }

    private func row(_ title: String, reading: ComputerInventoryReading<String>) -> some View {
        row(title, reading: reading) { $0 }
    }

    private func row<Value>(_ title: String, reading: ComputerInventoryReading<Value>,
                            format: (Value) -> String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: theme.panelGap) {
            Text(title).foregroundStyle(theme.muted)
            Spacer(minLength: theme.panelGap)
            Text(reading.value.map(format) ?? "Unavailable")
                .textSelection(.enabled).multilineTextAlignment(.trailing)
                .help(reading.key + (reading.failure.map { ": " + $0.description } ?? ""))
        }.font(.callout)
    }
}
