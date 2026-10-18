import SwiftUI

struct MetricSummary: View {
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    let reading: String
    let note: String
    var color: Color?
    var action: (() -> Void)?

    var body: some View {
        Group {
            if let action {
                Button(action: action) { content }.buttonStyle(.plain)
            } else {
                content
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                if let color {
                    Circle().fill(color).frame(width: 6, height: 6).accessibilityHidden(true)
                }
                Text(title).font(.caption).foregroundStyle(theme.muted)
                Spacer(minLength: 0)
                if action != nil {
                    Image(systemName: "arrow.up.right").font(.caption2)
                        .foregroundStyle(theme.muted).accessibilityHidden(true)
                }
            }
            Text(reading).font(theme.metricFont).monospacedDigit()
                .foregroundStyle(color ?? theme.text)
                .lineLimit(1).minimumScaleFactor(0.65)
            Text(note).font(.caption).foregroundStyle(theme.muted).lineLimit(1)
        }
        .frame(minWidth: 114, maxWidth: .infinity, alignment: .leading)
        .padding(theme.panelPadding)
        .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
        .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
    }
}

struct MetricSummaryGrid<Content: View>: View {
    @Environment(\.flightdeckTheme) private var theme
    @ViewBuilder var content: Content

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .top, spacing: theme.fieldGap) { content }
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)],
                      alignment: .leading, spacing: theme.fieldGap) { content }
        }
    }
}
