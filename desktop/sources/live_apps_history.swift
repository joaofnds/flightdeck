import SwiftUI

struct LiveAppHistoryCharts: View {
    @Environment(\.flightdeckTheme) private var theme
    let history: LiveAppHistoryView

    var body: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 330), alignment: .top)],
                alignment: .leading, spacing: theme.panelGap) {
                chart("CPU", model: history.cpu, unit: "% of one core",
                    explanation: "Known compatible process CPU deltas divided by their measured continuous interval. 100% uses one core. Unknown members are not treated as zero.")
                chart("Resident memory", model: history.resident, unit: "GiB",
                    explanation: "Known resident bytes of processes present at the selected observation. Shared pages count once per process, so this can exceed physical memory.")
                chart("Memory footprint", model: history.footprint, unit: "GiB",
                    explanation: "Known captured process footprint, kept separate from resident memory.")
                chart("Average power", model: history.power, unit: "W",
                    explanation: "Known process energy deltas divided by their measured continuous interval. App power is not the whole-machine power sensor.")
            }
            Text(history.coverage).font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func chart(_ title: String, model: TimeSeriesModel?, unit: String,
                       explanation: String) -> some View {
        Panel(title: title, explanation: explanation) {
            if let model {
                TimeSeriesPlot(model: model, style: TimeSeriesStyle(unit: unit, minimum: 0, fill: 0.1),
                    selectedTime: history.selectedTime, labels: history.labels)
                    .frame(height: theme.chartHeight)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(title + ", last 15 minutes")
                    .accessibilityValue(readings(model, unit: unit))
                ForEach(Array(model.lines.enumerated()), id: \.offset) { _, line in
                    HStack(spacing: theme.fieldGap) {
                        RoundedRectangle(cornerRadius: theme.controlRadius).fill(line.color)
                            .frame(width: theme.fieldGap, height: theme.chartStroke)
                        Text(line.name).lineLimit(1)
                        Spacer()
                        Text(dashboardReading(history.selectedValue(model: model, line: line), unit: line.unit ?? unit))
                            .monospacedDigit().foregroundStyle(theme.muted)
                    }.font(.caption)
                }
            } else {
                Text("No compatible App readings yet.").foregroundStyle(theme.muted)
                    .frame(maxWidth: .infinity, minHeight: theme.chartHeight, alignment: .center)
            }
        }
    }

    private func readings(_ model: TimeSeriesModel, unit: String) -> String {
        model.lines.map { line in
            line.name + " " + dashboardReading(history.selectedValue(model: model, line: line), unit: line.unit ?? unit)
        }.joined(separator: "; ")
    }
}
