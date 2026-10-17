import SwiftUI

struct LogicalCPUActivity: View {
    @Environment(\.flightdeckTheme) private var theme
    let readings: SelectedCPUReadings?
    let busy: Bool

    var body: some View {
        Panel(title: "Logical CPU activity", explanation: "Busy scheduler ticks divided by total ticks for each logical CPU over consecutive observations. 100% means one fully busy logical CPU. Indices are scheduler identifiers, not physical core or cluster assignments.") {
            if let readings, !readings.cpus.isEmpty {
                LazyVStack(spacing: theme.fieldGap) {
                    ForEach(Array(stride(from: 0, to: readings.cpus.count, by: 18)), id: \.self) { start in
                        CPUActivityRow(cpus: Array(readings.cpus[start..<min(start + 18, readings.cpus.count)]),
                            columns: min(18, readings.cpus.count))
                    }
                }
                Text(readings.unavailableReason ?? "100% is one logical CPU. Activity at the selected time.")
                    .font(theme.compactReadingFont).foregroundStyle(theme.muted)
            } else {
                Text(busy ? "Reading logical CPU activity…" : "Logical CPU readings unavailable.")
                    .font(theme.chartStateFont).foregroundStyle(theme.muted)
            }
        }
    }
}

private struct CPUActivityRow: View {
    @Environment(\.flightdeckTheme) private var theme
    let cpus: [LogicalCPUReading]
    let columns: Int

    var body: some View {
        Canvas { context, size in
            let column = size.width / Double(columns)
            for (index, cpu) in cpus.enumerated() {
                let center = (Double(index) + 0.5) * column
                context.draw(Text(CPUActivitySegments.readout(cpu.busyPercent))
                    .font(theme.compactReadingFont).foregroundStyle(theme.text),
                    at: CGPoint(x: center, y: 10))
                for segment in 0..<20 {
                    let rectangle = CGRect(x: Double(index) * column + 1,
                        y: 30 + Double(segment) * 10.5, width: max(1, column - 2), height: 8.8)
                    let lit = CPUActivitySegments.isLit(value: cpu.busyPercent, segment: segment)

                    context.fill(Path(roundedRect: rectangle, cornerRadius: 1.5),
                        with: .color(segmentColor(segment).opacity(lit ? 0.9 : 0.12)))
                }
                context.draw(Text("\(cpu.cpu)").font(theme.compactReadingFont)
                    .foregroundStyle(theme.muted), at: CGPoint(x: center, y: 254))
            }
        }
        .frame(height: 268)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Logical CPU activity")
        .accessibilityValue(cpus.map {
            "Logical CPU \($0.cpu), " + ($0.busyPercent == nil
                ? "unavailable" : CPUActivitySegments.readout($0.busyPercent))
        }.joined(separator: "; "))
    }

    private func segmentColor(_ segment: Int) -> Color {
        let level = (20 - segment) * 5 - 1

        if level >= 90 { return theme.cpuActivityPeak }
        if level >= 80 { return theme.cpuActivityHigh }
        if level >= 60 { return theme.cpuActivityModerate }

        return theme.cpuActivityLow
    }
}

enum CPUActivitySegments {
    static func isLit(value: Double?, segment: Int) -> Bool {
        precondition((0..<20).contains(segment))
        guard let value else { return false }

        let level = Double(20 - segment) * 5

        return value >= level || (level == 5 && value > 0)
    }

    static func readout(_ value: Double?) -> String {
        guard let value else { return "–" }

        return dashboardFixedReading(value, unit: "%", digits: 0)
    }
}
