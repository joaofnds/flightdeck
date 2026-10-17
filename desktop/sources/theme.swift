import AppKit
import SwiftUI

struct FlightdeckTheme {
    var background = Color(red: 0.067, green: 0.071, blue: 0.090)
    var panel = Color(red: 0.094, green: 0.106, blue: 0.122)
    var border = Color(red: 0.19, green: 0.20, blue: 0.23)
    var text = Color(red: 0.82, green: 0.82, blue: 0.87)
    var muted = Color(red: 0.56, green: 0.57, blue: 0.62)
    var navigationAccent = Palette.blue
    var focus = Palette.blue
    var warning = Palette.orange
    var cpuSeries = Palette.blue
    var gpuPowerSeries = Palette.purple
    var memoryActiveSeries = Palette.blue
    var memoryWiredSeries = Palette.orange
    var memoryCompressedSeries = Palette.purple
    var networkReceivedSeries = Palette.green
    var networkSentSeries = Palette.purple
    var systemPowerSeries = Palette.orange
    var temperatureSeries = Palette.red
    var metricFont = Font.system(size: 25, weight: .semibold, design: .rounded)
    var tableFontSize: CGFloat = 12
    var tableTypeface: NSFont {
        NSFont.monospacedDigitSystemFont(ofSize: tableFontSize, weight: .regular)
    }
    var tableFont: Font { .system(size: tableFontSize).monospacedDigit() }
    var titleFont = Font.system(size: 23, weight: .semibold)
    var navigationFont = Font.system(size: 13, weight: .medium)
    var brandFont = Font.system(size: 13, weight: .bold)
    var panelPadding: CGFloat = 18
    var contentPadding: CGFloat = 24
    var panelGap: CGFloat = 16
    var sectionGap: CGFloat = 22
    var fieldGap: CGFloat = 12
    var panelRadius: CGFloat = 7
    var controlRadius: CGFloat = 6
    var chartStroke: CGFloat = 1.5
    var chartHeight: CGFloat = 190

}

private enum Palette {
    static let blue = Color(red: 0.34, green: 0.58, blue: 0.95)
    static let green = Color(red: 0.45, green: 0.75, blue: 0.41)
    static let purple = Color(red: 0.72, green: 0.48, blue: 0.84)
    static let orange = Color(red: 1.0, green: 0.60, blue: 0.19)
    static let red = Color(red: 0.95, green: 0.29, blue: 0.34)
}

private struct ThemeKey: EnvironmentKey {
    static let defaultValue = FlightdeckTheme()
}

extension EnvironmentValues {
    var flightdeckTheme: FlightdeckTheme {
        get { self[ThemeKey.self] }
        set { self[ThemeKey.self] = newValue }
    }
}

struct Panel<Content: View>: View {
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    let explanation: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: theme.panelGap) {
            HStack {
                Text(title).font(.headline)
                Spacer()
                Image(systemName: "info.circle").foregroundStyle(theme.muted).help(explanation)
            }
            content
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .padding(theme.panelPadding)
        .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
        .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border, lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title)
    }
}

func metricText(_ value: Double?, unit: String, digits: Int = 1) -> String {
    guard let value, value.isFinite else { return "Unavailable" }
    return value.formatted(.number.precision(.fractionLength(digits))) + " " + unit
}
