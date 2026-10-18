import SwiftUI

struct FlightdeckDiagnostics {
    let catalog: Catalog?
    let collector: LiveCollectorSession.State
    let live: Bool
    let readerStatus: String
    let readerError: String?
    let busy: Bool

    var collectorStatus: String {
        guard live else { return "Saved source" }
        if catalog?.collectorCompleted == true { return "Stopped" }

        switch collector {
        case .idle: return "Not started"
        case .starting: return "Connecting"
        case .running: return "Connected"
        case .stopping: return "Stopping"
        case .stopped: return "Stopped"
        case .failed: return "Failed"
        }
    }

    var historyStatus: String {
        guard let catalog else { return "Unavailable" }
        if catalog.historyError != nil { return "Unavailable" }

        return catalog.windows.formatted() + " windows"
    }

    var readerReading: String { readerError == nil ? readerStatus : "Error" }

    var viewerStatus: String {
        if readerError != nil { return "Read failed" }

        return busy ? "Reading" : "Ready"
    }

    var lossStatus: String {
        guard live, let losses = catalog?.readerLosses else { return "Unavailable" }
        if losses.contains(where: { $0.losses.hasLosses }) { return "Loss reported" }

        return "None reported"
    }

    var collectorError: String? {
        if case .failed(let message) = collector { return message }

        return nil
    }
}

struct SelfMonitoringPane: View {
    @EnvironmentObject private var model: RecordingModel
    @EnvironmentObject private var collector: LiveCollectorSession
    @Environment(\.flightdeckTheme) private var theme
    @State private var detailsExpanded = true

    private var diagnostics: FlightdeckDiagnostics {
        FlightdeckDiagnostics(catalog: model.catalog, collector: collector.state,
            live: model.isLiveSource, readerStatus: model.liveStatus,
            readerError: model.error, busy: model.busy)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.sectionGap) {
            MetricSummaryGrid {
                MetricSummary(title: "Collector", reading: diagnostics.collectorStatus,
                    note: model.catalog?.latestCollectorPid.map { "PID \($0)" } ?? "Producer identity unavailable")
                MetricSummary(title: "Reader", reading: diagnostics.readerReading,
                    note: "Current connection")
                MetricSummary(title: "Saved history", reading: diagnostics.historyStatus,
                    note: "Committed windows")
                MetricSummary(title: "Reader losses", reading: diagnostics.lossStatus,
                    note: "Reported by this reader", color: diagnostics.lossStatus == "Loss reported"
                        ? theme.warning : nil)
            }
            Text("Connection metadata from the last reader response. Pausing live also freezes these observations.")
                .font(.caption).foregroundStyle(theme.muted)
            issues
            pipeline
            observation
            DisclosureGroup("Recording details", isExpanded: $detailsExpanded) {
                VStack(alignment: .leading, spacing: theme.fieldGap) {
                    row("Recording", value: model.path)
                    row("Ticks without wall time", value: model.catalog?.unknownClockTicks.formatted())
                    Text("CPU time, physical footprint, per-source collection cost and seal latency are not exposed by the current diagnostic connection.")
                        .font(.caption).foregroundStyle(theme.muted)
                }.padding(.top, theme.fieldGap)
            }.font(.headline)
        }
    }

    private var issues: some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            if let error = diagnostics.collectorError { issue("Collector", message: error) }
            if let error = diagnostics.readerError { issue("Reader", message: error) }
            if let error = model.catalog?.historyError { issue("Saved history", message: error) }
            ForEach(model.readerLossNotices, id: \.self) { notice in
                Label(notice, systemImage: "exclamationmark.triangle")
                    .font(.callout).foregroundStyle(theme.warning)
            }
        }
    }

    private var pipeline: some View {
        Panel(title: "Flightdeck pipeline",
              explanation: "Current collector connection and reader metadata. Connection status does not establish complete capture, durable history or a healthy persister.") {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 200), alignment: .leading)],
                      alignment: .leading, spacing: theme.panelGap) {
                stage("cpu", title: "Collector", value: diagnostics.collectorStatus,
                    detail: "macOS counters and process events")
                stage("memorychip", title: "Shared-memory reader", value: diagnostics.readerReading,
                    detail: "Reads the live unsealed tail")
                stage("externaldrive", title: "Saved history", value: diagnostics.historyStatus,
                    detail: "Immutable windows from the separate persister")
                stage("macwindow", title: "Native viewer", value: diagnostics.viewerStatus,
                    detail: "Rust queries · SwiftUI and Canvas")
            }
        }
    }

    private var observation: some View {
        Panel(title: "Reader position",
              explanation: "Latest catalog observation and selected chart observation are separate. Paused inspection does not move the collector. Publication numbers are meaningful only within their run and boot.") {
            if let latest = model.catalog?.latestObservation {
                row("Latest publication", value: latest.publication.formatted())
                row("Latest observed at", value: date(latest.wallNs))
                row("Run", value: latest.run)
                row("Boot", value: latest.boot)
            } else {
                Text("Latest producer observation unavailable").foregroundStyle(theme.muted)
            }
            Divider().overlay(theme.border)
            row("Selected publication", value: model.selection?.selected?.publication.formatted())
            row("Selected run", value: model.selection?.selected?.run)
            row("Selected boot", value: model.selection?.selected?.boot)
            row("Unsealed ticks retained by this reader", value: model.catalog?.tailTicks?.formatted())
        }
    }

    private func stage(_ icon: String, title: String, value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            Label(title, systemImage: icon).font(.headline)
            Text(value).font(.title3.weight(.medium)).foregroundStyle(theme.text)
            Text(detail).font(.caption).foregroundStyle(theme.muted)
        }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func issue(_ title: String, message: String) -> some View {
        Label(title + ": " + message, systemImage: "exclamationmark.triangle")
            .font(.callout).foregroundStyle(theme.warning).textSelection(.enabled)
    }

    private func row(_ title: String, value: String?) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: theme.panelGap) {
            Text(title).foregroundStyle(theme.muted)
            Spacer(minLength: theme.panelGap)
            Text(value ?? "Unavailable").monospacedDigit().textSelection(.enabled)
                .multilineTextAlignment(.trailing)
        }.font(.callout)
    }

    private func date(_ nanoseconds: UInt64) -> String {
        Date(timeIntervalSince1970: Double(nanoseconds) / 1e9)
            .formatted(.dateTime.hour().minute().second())
    }
}
