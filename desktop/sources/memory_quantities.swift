import SwiftUI

struct RecordedMemoryQuantities: View {
    @Environment(\.flightdeckTheme) private var theme
    @State private var expanded = true
    let selection: Selection

    var body: some View {
        let data = MemoryQuantitiesData(selection: selection)
        DisclosureGroup("All memory quantities", isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: theme.fieldGap) {
                if let date = data.date {
                    Text("Selected observation: \(date.formatted(.dateTime.hour().minute().second()))")
                        .font(.caption).foregroundStyle(theme.muted)
                } else {
                    Text("No observation selected.").font(.caption).foregroundStyle(theme.muted)
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), alignment: .leading)],
                          alignment: .leading, spacing: theme.fieldGap) {
                    ForEach(data.rows) { row in
                        HStack(spacing: theme.fieldGap) {
                            Text(row.name).foregroundStyle(theme.muted)
                            Spacer(minLength: 0)
                            Text(row.reading).monospacedDigit().foregroundStyle(theme.text)
                        }.font(theme.readingFont).lineLimit(1).minimumScaleFactor(0.7)
                    }
                }
                Text("Independent VM categories overlap and are not an additive used-memory total. Speculative is included in Free.")
                    .font(.callout).foregroundStyle(theme.muted)
            }.padding(.top, theme.fieldGap)
        }
        .font(.headline)
        .padding(theme.panelPadding)
        .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
        .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
    }
}

struct MemoryQuantitiesData {
    let selection: Selection

    var identity: RecordedObservationIdentity? { selection.selected?.identity }
    var date: Date? {
        selection.selected.map { Date(timeIntervalSince1970: Double($0.wallNs) / 1e9) }
    }

    var rows: [MemoryQuantityRow] {
        let summary = matchingSummary
        return [quantity("Installed", bytes: summary?.memoryTotalBytes),
            quantity("Active", bytes: summary?.memoryActiveBytes),
            quantity("Wired", bytes: summary?.memoryWiredBytes),
            quantity("Compressed", bytes: summary?.memoryCompressedBytes),
            quantity("Free", bytes: summary?.memoryFreeBytes),
            quantity("Inactive", bytes: summary?.memoryInactiveBytes),
            quantity("Speculative", bytes: summary?.memorySpeculativeBytes),
            quantity("Anonymous", bytes: summary?.memoryAnonymousBytes),
            quantity("File-backed", bytes: summary?.memoryFileBackedBytes),
            quantity("Purgeable", bytes: summary?.memoryPurgeableBytes),
            quantity("Swap used", bytes: summary?.swapUsedBytes),
            quantity("Swap allocated", bytes: summary?.swapTotalBytes),
            MemoryQuantityRow(name: "Memory pressure", reading: summary?.pressure
                .flatMap { MemoryPressureState(rawValue: $0) }?.label ?? "Unavailable")]
    }

    private var matchingSummary: Metric? {
        guard let selected = selection.selected, let summary = selection.summary,
              summary.identity == selected.identity else { return nil }

        return summary
    }

    private func quantity(_ name: String, bytes: UInt64?) -> MemoryQuantityRow {
        MemoryQuantityRow(name: name,
            reading: dashboardReading(bytes.map { Double($0) / 1_073_741_824 }, unit: "GiB"))
    }
}

struct MemoryQuantityRow: Equatable, Identifiable {
    let name: String
    let reading: String
    var id: String { name }
}
