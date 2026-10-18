import SwiftUI

struct RecordedStorageRates: Decodable, Sendable {
    let readBps: Double?
    let writeBps: Double?
    let readOpsPerSecond: Double?
    let writeOpsPerSecond: Double?
    let readErrorsPerSecond: Double?
    let writeErrorsPerSecond: Double?
    let readMeanMs: Double?
    let writeMeanMs: Double?
}

enum RecordedStorageState: String, Decodable, Sendable {
    case complete, failed, exhausted, partial

    var notice: String? {
        switch self {
        case .complete: nil
        case .failed: "The physical-disk scan failed. Readings remain unavailable."
        case .exhausted: "The physical-disk scan exceeded its capture bounds. Totals are incomplete."
        case .partial: "Some physical-disk readings are unavailable. Totals are incomplete."
        }
    }
}

struct RecordedStorageReading: Decodable, Sendable {
    let state: RecordedStorageState
    let rates: RecordedStorageRates
    let devices: [RecordedStorageDevice]

    private enum CodingKeys: String, CodingKey { case state, devices }

    init(from decoder: Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        state = try fields.decode(RecordedStorageState.self, forKey: .state)
        devices = try fields.decode([RecordedStorageDevice].self, forKey: .devices)
        rates = try RecordedStorageRates(from: decoder)
    }
}

struct RecordedStorageDevice: Decodable, Identifiable, Sendable {
    let driverId: UInt64
    let mediaId: UInt64
    let name: String
    let rates: RecordedStorageRates
    var id: String { "\(driverId):\(mediaId)" }

    private enum CodingKeys: String, CodingKey { case driverId, mediaId, name }

    init(from decoder: Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        driverId = try fields.decode(UInt64.self, forKey: .driverId)
        mediaId = try fields.decode(UInt64.self, forKey: .mediaId)
        name = try fields.decode(String.self, forKey: .name)
        rates = try RecordedStorageRates(from: decoder)
    }
}

struct RecordedStorageData {
    let context: RecordedChartContext
    let theme: FlightdeckTheme

    var selected: RecordedStorageReading? {
        guard let observation = context.selection.selected,
              let summary = context.selection.summary,
              summary.identity == observation.identity,
              summary.wallNs == observation.wallNs else { return nil }

        return summary.storage
    }

    var throughput: [TimeSeriesLine] {
        [line("Read", color: theme.storageReadSeries, metric: \.readBps, scale: 1_000_000),
         line("Write", color: theme.storageWriteSeries, metric: \.writeBps,
            scale: 1_000_000, negative: true)]
    }

    var operations: [TimeSeriesLine] {
        [line("Read", color: theme.storageReadSeries, metric: \.readOpsPerSecond),
         line("Write", color: theme.storageWriteSeries, metric: \.writeOpsPerSecond, negative: true)]
    }

    var meanTime: [TimeSeriesLine] {
        [line("Read", color: theme.storageReadSeries, metric: \.readMeanMs),
         line("Write", color: theme.storageWriteSeries, metric: \.writeMeanMs, pattern: .dashed)]
    }

    var errors: [TimeSeriesLine] {
        [line("Read errors", color: theme.storageReadErrorsSeries, metric: \.readErrorsPerSecond),
         line("Write errors", color: theme.storageWriteErrorsSeries,
            metric: \.writeErrorsPerSecond, pattern: .dashed)]
    }

    private func line(_ name: String, color: Color, metric: KeyPath<RecordedStorageRates, Double?>,
                      scale: Double = 1, negative: Bool = false,
                      pattern: TimeSeriesStrokePattern = .solid) -> TimeSeriesLine {
        TimeSeriesLine(name: name, color: color,
            values: context.selection.trends.map { $0.storage?.rates[keyPath: metric].map { $0 / scale } },
            negative: negative, strokePattern: pattern)
    }
}

struct RecordedStorage: View {
    @EnvironmentObject private var model: RecordingModel
    @Environment(\.flightdeckTheme) private var theme
    @State private var inspectedID: String?
    @State private var inspecting = false
    @State private var historyExpanded = true
    @State private var devicesExpanded = true

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            if let selection = model.selection {
                let data = RecordedStorageData(context: RecordedChartContext(selection: selection),
                    theme: theme)
                if selection.selected == nil {
                    Label("No recorded tick at this second", systemImage: "clock.badge.questionmark")
                        .foregroundStyle(theme.muted)
                } else if let notice = data.selected?.state.notice {
                    Label(notice, systemImage: "exclamationmark.triangle")
                        .font(.callout).foregroundStyle(theme.warning)
                } else if data.selected == nil {
                    Label("This source does not include physical-disk readings.", systemImage: "externaldrive")
                        .font(.callout).foregroundStyle(theme.muted)
                }
                summaries(data)
                DisclosureGroup("Storage history", isExpanded: $historyExpanded) {
                    history(data).padding(.top, theme.fieldGap)
                }.font(.headline)
                DisclosureGroup("Physical disks", isExpanded: $devicesExpanded) {
                    devices(data).padding(.top, theme.fieldGap)
                }.font(.headline)
            } else {
                Text(model.busy ? "Reading selected tick…" : "No observation selected.")
                    .foregroundStyle(theme.muted)
            }
        }
        .onChange(of: model.selection?.selected?.identity) { _, _ in resetInspection() }
        .onChange(of: model.selection?.second) { _, _ in resetInspection() }
    }

    private func resetInspection() { inspectedID = nil; inspecting = false }

    private func summaries(_ data: RecordedStorageData) -> some View {
        MetricSummaryGrid {
            MetricSummary(title: "Disk read", reading: dashboardReading(data.selected?.rates.readBps.map {
                $0 / 1_000_000
            }, unit: "MB/s"), note: "Physical devices", color: theme.storageReadSeries)
            MetricSummary(title: "Disk write", reading: dashboardReading(data.selected?.rates.writeBps.map {
                $0 / 1_000_000
            }, unit: "MB/s"), note: "Physical devices", color: theme.storageWriteSeries)
            MetricSummary(title: "Read operations", reading: dashboardReading(
                data.selected?.rates.readOpsPerSecond, unit: "ops/s"), note: "Completed requests",
                color: theme.storageReadSeries)
            MetricSummary(title: "Write operations", reading: dashboardReading(
                data.selected?.rates.writeOpsPerSecond, unit: "ops/s"), note: "Completed requests",
                color: theme.storageWriteSeries)
        }
    }

    private func history(_ data: RecordedStorageData) -> some View {
        VStack(spacing: theme.panelGap) {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top, spacing: theme.panelGap) {
                    throughput(data).frame(minWidth: 330)
                    operations(data).frame(minWidth: 330)
                }
                VStack(spacing: theme.panelGap) { throughput(data); operations(data) }
            }
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top, spacing: theme.panelGap) {
                    meanTime(data).frame(minWidth: 330)
                    errors(data).frame(minWidth: 330)
                }
                VStack(spacing: theme.panelGap) { meanTime(data); errors(data) }
            }
        }
    }

    static func throughputChart(_ data: RecordedStorageData, inspectedID: Binding<String?>,
                                inspecting: Binding<Bool>) -> RecordedResourceChart {
        RecordedResourceChart(title: "Disk throughput", context: data.context,
            lines: data.throughput, style: TimeSeriesStyle(unit: "MB/s", fill: 0.1),
            inspectedID: inspectedID, inspecting: inspecting,
            explanation: "Bytes transferred by physical block devices per second. Writes are mirrored below zero; inspected values remain positive. Disk images are excluded. Totals require complete matching devices and valid counter pairs for every device.")
    }

    static func operationsChart(_ data: RecordedStorageData, inspectedID: Binding<String?>,
                                inspecting: Binding<Bool>) -> RecordedResourceChart {
        RecordedResourceChart(title: "Disk operations", context: data.context,
            lines: data.operations, style: TimeSeriesStyle(unit: "ops/s", fill: 0.1),
            inspectedID: inspectedID, inspecting: inspecting,
            explanation: "Block-driver requests completed per second, including error or short-transfer completions. Writes are mirrored; inspected values stay positive. These are device requests, separate from process I/O calls.")
    }

    private func throughput(_ data: RecordedStorageData) -> some View {
        Self.throughputChart(data, inspectedID: $inspectedID, inspecting: $inspecting)
    }

    private func operations(_ data: RecordedStorageData) -> some View {
        Self.operationsChart(data, inspectedID: $inspectedID, inspecting: $inspecting)
    }

    private func meanTime(_ data: RecordedStorageData) -> some View {
        RecordedResourceChart(title: "Mean I/O time", context: data.context,
            lines: data.meanTime, style: TimeSeriesStyle(unit: "ms", minimum: 0, fill: 0.1),
            inspectedID: $inspectedID, inspecting: $inspecting,
            explanation: "Total completed-request duration divided by completed operations, weighted across devices. It includes time within the block driver and is not queue wait, a latency percentile or busy percentage. An interval without completed requests has no mean.")
    }

    private func errors(_ data: RecordedStorageData) -> some View {
        RecordedResourceChart(title: "Disk errors", context: data.context,
            lines: data.errors, style: TimeSeriesStyle(unit: "errors/s", minimum: 0),
            inspectedID: $inspectedID, inspecting: $inspecting,
            explanation: "Changes in the block drivers’ reported read and write error counters per second. Missing or reset counters leave gaps. A measured zero means no new errors in that interval.")
    }

    private func devices(_ data: RecordedStorageData) -> some View {
        VStack(alignment: .leading, spacing: theme.panelGap) {
            if let selected = data.selected {
                if selected.devices.isEmpty {
                    Text(selected.state == .complete ? "No physical disks reported at this tick."
                        : "No device inventory available at this tick.")
                        .font(.callout).foregroundStyle(theme.muted)
                }
                ForEach(selected.devices) { device in RecordedStorageDeviceRow(device: device) }
            } else {
                Text("No device inventory recorded.").font(.callout).foregroundStyle(theme.muted)
            }
        }
    }
}

private struct RecordedStorageDeviceRow: View {
    @Environment(\.flightdeckTheme) private var theme
    let device: RecordedStorageDevice

    var body: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            Label(device.name, systemImage: "internaldrive").font(.headline)
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(minimum: 120),
                alignment: .leading), count: 4),
                      alignment: .leading, spacing: theme.fieldGap) {
                reading("Read", value: device.rates.readBps.map { $0 / 1_000_000 }, unit: "MB/s")
                reading("Write", value: device.rates.writeBps.map { $0 / 1_000_000 }, unit: "MB/s")
                reading("Read operations", value: device.rates.readOpsPerSecond, unit: "ops/s")
                reading("Write operations", value: device.rates.writeOpsPerSecond, unit: "ops/s")
                reading("Read mean", value: device.rates.readMeanMs, unit: "ms")
                reading("Write mean", value: device.rates.writeMeanMs, unit: "ms")
                reading("Read errors", value: device.rates.readErrorsPerSecond, unit: "errors/s")
                reading("Write errors", value: device.rates.writeErrorsPerSecond, unit: "errors/s")
            }
        }
        .padding(theme.panelPadding)
        .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
        .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Physical disk " + device.name)
    }

    private func reading(_ title: String, value: Double?, unit: String) -> some View {
        VStack(alignment: .leading, spacing: theme.fieldGap / 2) {
            Text(title).font(.caption).foregroundStyle(theme.muted)
            Text(dashboardReading(value, unit: unit)).font(theme.readingFont).monospacedDigit()
                .foregroundStyle(theme.text)
        }
    }
}
