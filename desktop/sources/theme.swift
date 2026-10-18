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
    var cpuActivityLow = Palette.green
    var cpuActivityModerate = Color(red: 0.95, green: 0.80, blue: 0.05)
    var cpuActivityHigh = Palette.orange
    var cpuActivityPeak = Palette.red
    var gpuActivitySeries = Palette.purple
    var gpuPowerSeries = Palette.purple
    var memoryActiveSeries = Palette.blue
    var memoryWiredSeries = Palette.orange
    var memoryCompressedSeries = Palette.purple
    var swapUsedSeries = Palette.purple
    var swapAllocatedSeries = Color(red: 0.82, green: 0.82, blue: 0.87)
    var swapReadSeries = Palette.purple
    var swapWriteSeries = Palette.orange
    var memoryPageinsSeries = Palette.blue
    var memoryCompressionsSeries = Palette.orange
    var memoryDecompressionsSeries = Palette.purple
    var memoryPressureNormal = Palette.green
    var memoryPressureWarning = Color(red: 0.95, green: 0.80, blue: 0.05)
    var memoryPressureCritical = Palette.red
    var networkReceivedSeries = Palette.green
    var networkSentSeries = Palette.purple
    var networkReceivedPacketsSeries = Palette.green
    var networkSentPacketsSeries = Palette.purple
    var networkReceiveErrorsSeries = Palette.red
    var networkSendErrorsSeries = Palette.orange
    var storageReadSeries = Color(red: 0.95, green: 0.80, blue: 0.05)
    var storageWriteSeries = Palette.orange
    var storageReadErrorsSeries = Palette.red
    var storageWriteErrorsSeries = Palette.orange
    var systemPowerSeries = Palette.orange
    var batteryChargeSeries = Palette.green
    var batteryFlowSeries = Palette.orange
    var batteryHealth = Palette.green
    var batteryCharging = Palette.green
    var batteryNotCharging = Palette.purple
    var temperatureSeries = Palette.red
    var fanSeries = [
        Palette.blue, Palette.orange, Palette.purple, Palette.green, Palette.red,
        Color(red: 0.95, green: 0.80, blue: 0.05),
        Color(red: 0.54, green: 0.72, blue: 1.0),
        Color(red: 0.79, green: 0.58, blue: 0.90),
        Color(red: 0.22, green: 0.53, blue: 0.18),
        Color(red: 0.76, green: 0.36, blue: 0.09)
    ]
    var airflowCool = Palette.blue
    var airflowWarm = Color(red: 0.95, green: 0.80, blue: 0.05)
    var airflowHot = Palette.red
    var schematicMemory = Palette.green
    var thermalNominal = Palette.green
    var neutralSeries = Color(red: 0.56, green: 0.57, blue: 0.62)
    var lightBlueSeries = Color(red: 0.54, green: 0.72, blue: 1.0)
    var lightPurpleSeries = Color(red: 0.79, green: 0.58, blue: 0.90)
    var chartPalette = [
        Color(red: 0.45, green: 0.75, blue: 0.41),
        Color(red: 0.95, green: 0.80, blue: 0.05),
        Palette.blue, Palette.orange, Palette.red, Palette.purple,
        Color(red: 0.22, green: 0.53, blue: 0.18),
        Color(red: 0.88, green: 0.71, blue: 0.00),
        Color(red: 0.12, green: 0.38, blue: 0.77),
        Color(red: 0.76, green: 0.36, blue: 0.09)
    ]
    var metricFont = Font.system(size: 25, weight: .semibold, design: .rounded)
    var tableFontSize: CGFloat = 12
    var tableTypeface: NSFont {
        NSFont.monospacedDigitSystemFont(ofSize: tableFontSize, weight: .regular)
    }
    var tableFont: Font { .system(size: tableFontSize).monospacedDigit() }
    var titleFont = Font.system(size: 23, weight: .semibold)
    var navigationFont = Font.system(size: 13, weight: .medium)
    var brandFont = Font.system(size: 13, weight: .bold)
    var chartAxisFont = Font.system(size: 10)
    var chartSecondaryAxisFont = Font.system(size: 9)
    var compactReadingFont = Font.system(size: 11)
    var chartStateFont = Font.system(size: 12)
    var readingFont = Font.system(size: 13)
    var schematicLabelFont = Font.system(size: 11, weight: .medium)
    var schematicValueFont = Font.system(size: 20, weight: .medium)
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
