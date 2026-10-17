import SwiftUI

struct ReferenceSelfMonitoring: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme

    var body: some View {
        if let snapshot = model.snapshot {
            let capture = snapshot.selfCapture
            VStack(alignment: .leading, spacing: theme.sectionGap) {
                introduction(capture)
                summaries(capture)
                pipeline
                HStack {
                    Text("Collection and overhead").font(.title3.weight(.semibold))
                    Spacer()
                    Text("Run 0:00–" + diagnosticElapsed(capture.duration))
                        .font(.callout).foregroundStyle(theme.muted)
                }
                reportControl(capture)
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 360))], spacing: theme.panelGap) {
                    ForEach(snapshot.selfPanels) { panel in ReferenceChart(panel: panel) }
                }
                sourceCosts(capture)
                seals(capture)
                missingSignals
                reportTable(capture)
                minuteTable(capture)
            }
        }
    }

    private func introduction(_ capture: ReferenceSelfCapture) -> some View {
        VStack(alignment: .leading, spacing: theme.fieldGap) {
            Text("Saved Flightdeck diagnostic run · 5:00 elapsed · ten-second reports · no live connection")
                .font(.callout).foregroundStyle(theme.muted)
            Label("\(capture.tracking.missed) processes missed during this run",
                  systemImage: "exclamationmark.triangle").font(.headline).foregroundStyle(theme.warning)
            Text("\(capture.tracking.found) found of \(capture.tracking.started) starts in the run-end PID-sequence estimate. No event-list drops were reported. Query and viewer health were not captured.")
                .font(.callout)
        }.padding(theme.panelPadding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(theme.warning.opacity(0.08),
                        in: RoundedRectangle(cornerRadius: theme.panelRadius))
    }

    private func summaries(_ capture: ReferenceSelfCapture) -> some View {
        let partial = capture.seals.filter { $0.end - $0.first < 60 }.count
        let slips = capture.reports.reduce(UInt64(0)) { $0 + $1.slips }
        let publications = capture.seals.reduce(UInt64(0)) { $0 + $1.loss }
        let events = capture.seals.reduce(UInt64(0)) { $0 + $1.eventsLost }
        return LazyVGrid(columns: [GridItem(.adaptive(minimum: 190))], spacing: theme.panelGap) {
            diagnosticSummary("Recording CPU",
                value: metricText(capture.minutes.last?.combinedPercent, unit: "%", digits: 2),
                note: "Last complete minute · 5% of one core budget")
            diagnosticSummary("Scheduled tick slips", value: String(slips),
                note: "Across 30 ten-second reports")
            diagnosticSummary("Sealed windows", value: String(capture.seals.count),
                note: "\(capture.seals.count - partial) full windows + \(partial) partial window")
            diagnosticSummary("Lost publications / events", value: "\(publications) / \(events)",
                note: "Reported by seals; unknown-extent flag not captured")
        }
    }

    private func diagnosticSummary(_ title: String, value: String, note: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title).font(.caption).foregroundStyle(theme.muted)
            Text(value).font(theme.metricFont).monospacedDigit()
            Text(note).font(.caption).foregroundStyle(theme.muted)
        }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(theme.panelPadding)
            .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
            .accessibilityElement(children: .combine)
    }

    private var pipeline: some View {
        Panel(title: "Flightdeck recording pipeline",
              explanation: "The saved run describes the collector and separate persister; no external metrics service is involved.") {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 200))], alignment: .leading,
                      spacing: theme.panelGap) {
                pipelineStage("Collector", "macOS readings and process events. Scheduled once a second; measures source CPU cost.")
                pipelineStage("Shared-memory ring", "120 tick slots plus a separate event log. One writer; readers can lose overwritten records.")
                pipelineStage("Persister", "A separate process reads the ring and commits bounded windows. Collection continues if it exits.")
                pipelineStage("Sealed recording", "Immutable Parquet windows of 60 elapsed seconds. Manifests retain availability, losses and clock bounds.")
            }
            Text("Sealed recording → disk query API → native viewer. The query API and viewer exist today; this historical run sampled neither query performance nor viewer health. These paths use no metrics server or dashboard service.")
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func pipelineStage(_ title: String, _ description: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            Text(description).font(.callout).foregroundStyle(theme.muted)
        }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func reportControl(_ capture: ReferenceSelfCapture) -> some View {
        HStack(spacing: theme.panelGap) {
            if capture.reports.indices.contains(model.selfIndex) {
                Text("Inspecting report ending " + diagnosticElapsed(capture.reports[model.selfIndex].second))
                    .monospacedDigit()
            }
            Spacer()
            Stepper("Report", value: $model.selfIndex, in: 0...(capture.reports.count - 1))
                .labelsHidden().accessibilityLabel("Inspect diagnostic report")
            Button("Latest report") { model.selfIndex = capture.reports.count - 1 }
        }.font(.callout)
    }

    private func sourceCosts(_ capture: ReferenceSelfCapture) -> some View {
        Panel(title: "CPU time by collection source",
              explanation: "Ten-second report at the selected elapsed time. Milliseconds of CPU per elapsed second; not sensor-read wall time.") {
            if capture.reports.indices.contains(model.selfIndex) {
                let costs = capture.reports[model.selfIndex].costs.sorted {
                    $0.value == $1.value ? $0.key < $1.key : $0.value > $1.value
                }
                let maximum = costs.first?.value ?? 0
                ForEach(costs, id: \.key) { name, value in
                    HStack(spacing: theme.fieldGap) {
                        Text(name == "ioreport" ? "IOReport" : name == "smc" ? "SMC" : name)
                            .frame(width: 115, alignment: .leading)
                        GeometryReader { geometry in
                            Rectangle().fill(theme.cpuSeries)
                                .frame(width: maximum > 0 ? geometry.size.width * value / maximum : 0)
                        }.frame(height: 6)
                        Text(metricText(value, unit: "ms/s", digits: 2))
                            .monospacedDigit().frame(width: 100, alignment: .trailing)
                    }.font(.caption).accessibilityElement(children: .combine)
                }
            }
            Text("Source values are rounded independently. Unattributed covers remaining collector work.")
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func seals(_ capture: ReferenceSelfCapture) -> some View {
        Panel(title: "Sealed windows",
              explanation: "Completed seals, including stop/drain. Publication ranges are half-open. Staged bytes are uncompressed in-memory window data, not disk size.") {
            ReferenceDiagnosticTable(headings: ["Window", "Publications", "Seal time", "Staged data",
                                                "Lost publications", "Lost events"],
                rows: capture.seals.map { seal in
                    [String(seal.window) + (seal.end - seal.first < 60 ? " · partial" : ""),
                     "\(seal.first)…\(seal.end)",
                     metricText(Double(seal.sealNs) / 1_000_000, unit: "ms", digits: 1),
                     metricText(Double(seal.stagedBytes) / 1_048_576, unit: "MiB", digits: 2),
                     String(seal.loss), String(seal.eventsLost)]
                })
            Text("Persister exit status \(capture.drain.status) after a \(metricText(capture.drain.seconds, unit: "s", digits: 2)) drain. These outcomes describe this saved run only. Zero reported seal losses do not establish complete history: the text logs omitted the unknown-extent flag.")
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private var missingSignals: some View {
        Panel(title: "Signals still to connect",
              explanation: "Unavailable instrumentation remains explicit; it is not a measured healthy state.") {
            pipelineStage("Collector memory · Not captured", "Collector resident memory and physical footprint were not logged. Persister footprint does not include the collector.")
            pipelineStage("Ring reader lag · Not captured", "Latest/consumed publication and event cursors, oldest retained record and overwrite distance. Capacity is not occupancy.")
            pipelineStage("Recording freshness · Not captured", "Last durable observation, time since last seal, unknown-extent losses, disk bytes/free space and write failures.")
            pipelineStage("Query performance · Not captured", "Selected-second latency, coverage, decoded bytes, failures and query memory. This saved run did not exercise queries.")
            pipelineStage("Native viewer · Not captured", "Viewer CPU/footprint, selected-second display time, rendering cost and live-reader lag. The viewer now exists; its health was not measured in this older run.")
        }
    }

    private func reportTable(_ capture: ReferenceSelfCapture) -> some View {
        ReferenceDiagnosticDisclosure(title: "Captured report data") {
            ReferenceDiagnosticTable(headings: ["Report ends", "Collector CPU", "Collector power",
                "Average lateness", "Maximum lateness", "Slips", "Observed", "Usage refused",
                "Gone", "Event-list drops"], rows: capture.reports.map { report in
                    [diagnosticElapsed(report.second), metricText(report.cpu, unit: "%", digits: 2),
                     metricText(report.powerMw, unit: "mW", digits: 2),
                     metricText(report.late, unit: "ms"), metricText(report.maximum, unit: "ms"),
                     String(report.slips), String(report.processes), String(report.refused),
                     String(report.gone), String(report.dropped)]
                })
            Text("Collector power is its reported process energy divided by the report interval. It excludes the persister, viewer and whole-machine power.")
                .font(.caption).foregroundStyle(theme.muted)
        }
    }

    private func minuteTable(_ capture: ReferenceSelfCapture) -> some View {
        ReferenceDiagnosticDisclosure(title: "CPU budget intervals and capture provenance") {
            ReferenceDiagnosticTable(headings: ["Minute ends", "Collector CPU", "Persister CPU",
                "Combined CPU", "Elapsed ns", "Current footprint", "Lifetime maximum"],
                rows: capture.minutes.map { minute in
                    let footprint = capture.footprints.first { $0.second == minute.second }
                    return [diagnosticElapsed(minute.second),
                        metricText(minute.collector, unit: "%", digits: 3),
                        metricText(minute.persister, unit: "%", digits: 3),
                        metricText(minute.combinedPercent, unit: "%", digits: 3),
                        String(minute.elapsedNs),
                        metricText(footprint.map { Double($0.currentFootprintBytes) / 1_048_576 },
                                   unit: "MiB", digits: 2),
                        metricText(footprint.map { Double($0.lifetimeFootprintBytesMax) / 1_048_576 },
                                   unit: "MiB", digits: 2)]
                })
            Text("Saved TASK-19 output from build \(capture.buildCommit). This is historical reference data, not a measurement of the current build. Wall time was not retained. Nominal report ends come from scheduled tick numbers; minute CPU uses each reported elapsed_ns. No observations are interpolated into the unrelated machine capture.")
                .font(.caption).foregroundStyle(theme.muted)
            ReferenceDiagnosticTable(headings: ["Source path", "SHA-256"],
                rows: capture.provenance.map { [$0.path, $0.sha256] })
            Text("Captured binary hashes").font(.headline)
            Text(capture.buildBinaryHashes).font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
        }
    }
}

private struct ReferenceDiagnosticDisclosure<Content: View>: View {
    @Environment(\.flightdeckTheme) private var theme
    let title: String
    @ViewBuilder var content: Content
    @State private var expanded = true

    var body: some View {
        DisclosureGroup(title, isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: theme.fieldGap) { content }.padding(.top, theme.fieldGap)
        }.font(.headline).padding(theme.panelPadding)
            .background(theme.panel, in: RoundedRectangle(cornerRadius: theme.panelRadius))
            .overlay(RoundedRectangle(cornerRadius: theme.panelRadius).stroke(theme.border))
    }
}

private struct ReferenceDiagnosticTable: View {
    @Environment(\.flightdeckTheme) private var theme
    let headings: [String]
    let rows: [[String]]

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: theme.panelGap,
                 verticalSpacing: theme.fieldGap) {
                GridRow {
                    ForEach(Array(headings.enumerated()), id: \.offset) { _, heading in
                        Text(heading).fontWeight(.semibold).foregroundStyle(theme.muted)
                    }
                }
                Divider().gridCellUnsizedAxes(.horizontal)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, value in
                            Text(value).monospacedDigit().textSelection(.enabled)
                        }
                    }.accessibilityElement(children: .combine)
                }
            }.font(theme.tableFont).padding(.bottom, 6)
        }
    }
}

private func diagnosticElapsed(_ second: UInt64) -> String {
    "\(second / 60):" + String(format: "%02llu", second % 60)
}
