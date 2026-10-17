import SwiftUI

struct ReferenceWindow: Equatable {
    private(set) var lower = 90
    private(set) var upper = 120
    private(set) var selected = 120

    mutating func select(_ index: Int) {
        selected = min(upper, max(lower, index))
    }

    mutating func selectCell(fraction: Double) {
        precondition(fraction.isFinite)
        let clamped = min(1, max(0, fraction))
        select(lower + Int(floor(clamped * Double(upper - lower + 1))))
    }

    mutating func range(lower: Int, upper: Int) {
        precondition((0...120).contains(lower))
        precondition((lower...120).contains(upper))
        self.lower = lower
        self.upper = upper
        selected = upper
    }
}

@MainActor
final class ReferenceModel: ObservableObject {
    @Published var snapshot: ReferenceSnapshot?
    @Published var error: String?
    @Published var route = "overview"
    @Published var window = ReferenceWindow()
    @Published var app = "Brave Browser"
    @Published var process = "*"
    @Published var selfIndex = 29
    let schematicPreview = SchematicPreviewState()

    init() {
        do { snapshot = try ReferenceSnapshot.load() }
        catch { self.error = error.localizedDescription }
    }

    init(snapshot: ReferenceSnapshot) {
        self.snapshot = snapshot
    }

    func inspect(sample: Int) {
        let selected = min(window.upper, max(window.lower, sample))
        guard selected != window.selected else { return }

        window.select(selected)
    }

    func inspectCell(fraction: Double) {
        var candidate = window
        candidate.selectCell(fraction: fraction)
        guard candidate != window else { return }

        window = candidate
    }

    func inspectDiagnostic(report: Int) {
        let selected = min(29, max(0, report))
        guard selected != selfIndex else { return }

        selfIndex = selected
    }

    func value(_ key: String, index: Int? = nil) -> Double? {
        guard let values = snapshot?.series[key]?.first?.values else { return nil }

        let index = index ?? window.selected
        return values.indices.contains(index) ? values[index] : nil
    }

    func date(_ index: Int) -> Date {
        guard let snapshot else { return .distantPast }

        return Date(timeIntervalSince1970: Double(snapshot.start) + Double(index) * Double(snapshot.step))
    }

    func select(date: Date) {
        guard let snapshot else { return }

        let offset = (date.timeIntervalSince1970 - Double(snapshot.start)) / Double(snapshot.step)
        guard offset.isFinite else { return }

        inspect(sample: Int(min(120, max(0, offset.rounded()))))
    }

    func inspect(app: String) {
        self.app = app
        process = "*"
        route = "apps"
    }
}

struct ApplicationConsole: View {
    @EnvironmentObject private var recording: RecordingModel
    @StateObject private var preview = ReferenceModel()

    var body: some View {
        Group {
            if recording.showsReference {
                ReferenceConsole().environmentObject(preview)
            } else {
                Console()
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Picker("Data source", selection: $recording.showsReference) {
                    Text("Captured prototype reference").tag(true)
                    Text("Flightdeck recording").tag(false)
                }.pickerStyle(.menu).frame(maxWidth: 260)
            }
        }
    }
}

struct ReferenceConsole: View {
    @EnvironmentObject private var model: ReferenceModel
    @Environment(\.flightdeckTheme) private var theme
    @State private var timePicker = false
    @State private var from = Date.distantPast
    @State private var to = Date.distantPast

    static let destinations: [(String, String, String)] = [
        ("overview", "Overview", "square.grid.2x2"),
        ("cpu", "CPU & Processes", "cpu"), ("apps", "Apps", "app.dashed"),
        ("gpu", "GPU", "rectangle.3.group"), ("memory", "Memory", "memorychip"),
        ("energy", "Energy", "bolt"), ("storage", "Storage", "internaldrive"),
        ("network", "Network", "network"), ("battery", "Battery", "battery.75percent"),
        ("computer", "Computer", "laptopcomputer"), ("self", "Self-monitoring", "waveform.path.ecg")
    ]

    var body: some View {
        NavigationSplitView {
            VStack(alignment: .leading, spacing: 4) {
                Label("FLIGHTDECK", systemImage: "waveform.path.ecg")
                    .font(theme.brandFont).tracking(1.4).padding(.bottom, theme.panelGap)
                ScrollView {
                    VStack(spacing: 4) {
                        ForEach(Self.destinations, id: \.0) { id, title, icon in
                            if id == "apps" || id == "computer" { Divider().padding(.vertical, 8) }
                            Button { model.route = id } label: {
                                Label(title, systemImage: icon).font(theme.navigationFont)
                                    .frame(maxWidth: .infinity, alignment: .leading).padding(11)
                                    .background(model.route == id ? theme.navigationAccent.opacity(0.14) : .clear,
                                        in: RoundedRectangle(cornerRadius: theme.controlRadius))
                                    .foregroundStyle(model.route == id ? theme.navigationAccent : theme.text)
                            }.buttonStyle(.plain)
                        }
                    }
                }
                Text("Captured reference · not live")
                    .font(.caption).foregroundStyle(theme.warning).padding(.top, theme.fieldGap)
            }.padding(theme.panelPadding).background(theme.background)
                .navigationSplitViewColumnWidth(min: 180, ideal: 210, max: 250)
        } detail: {
            VStack(spacing: 0) {
                header
                Divider()
                ScrollView {
                    if let error = model.error {
                        ContentUnavailableView("Reference unavailable", systemImage: "externaldrive.badge.exclamationmark",
                            description: Text(error)).padding(theme.contentPadding)
                    } else {
                        ReferencePages().id(model.route).padding(theme.contentPadding)
                    }
                }
                HStack {
                    Text(model.route == "self" ? "Saved Flightdeck diagnostic run · elapsed time · not live" : "Captured sysmon hour · 30-second samples · rates use 1-minute windows · not live")
                    Spacer()
                    if model.route != "self", model.route != "computer" {
                        Text(model.date(model.window.selected), format: .dateTime.hour().minute().second())
                    }
                }.font(.caption).foregroundStyle(theme.muted).padding(theme.fieldGap)
                    .background(theme.panel)
            }.background(theme.background).foregroundStyle(theme.text)
        }
        .navigationTitle("Flightdeck")
        .tint(theme.focus).preferredColorScheme(.dark)
    }

    private var header: some View {
        HStack(spacing: theme.panelGap) {
            VStack(alignment: .leading, spacing: 5) {
                Text(Self.destinations.first { $0.0 == model.route }?.1 ?? "Overview").font(theme.titleFont)
                Text(model.route == "self" ? "Flightdeck collector → ring → persister → recording → viewer" : "Captured prototype reference · not connected to the live Mac")
                    .font(.caption).foregroundStyle(theme.muted)
            }
            Spacer()
            if model.route != "computer", model.route != "self" {
                Button {
                    from = model.date(model.window.lower)
                    to = model.date(model.window.upper)
                    timePicker.toggle()
                } label: {
                    Label(model.window.lower == 90 && model.window.upper == 120 ? "Last 15 minutes" : model.window.lower == 0 && model.window.upper == 120 ? "Last 1 hour" : "Custom range", systemImage: "clock")
                }.popover(isPresented: $timePicker) { rangePicker }
                Button { model.inspect(sample: model.window.upper) } label: {
                    Image(systemName: "arrow.uturn.forward")
                }.help("Latest captured sample").accessibilityLabel("Latest captured sample")
                Text(model.date(model.window.selected), format: .dateTime.hour().minute().second())
                    .monospacedDigit().font(.callout)
            }
        }.padding(.horizontal, theme.contentPadding).padding(.vertical, theme.panelPadding)
    }

    private var rangePicker: some View {
        VStack(alignment: .leading, spacing: theme.panelGap) {
            Text("Captured time range").font(.headline)
            Button("Last 15 minutes") { model.window.range(lower: 90, upper: 120); timePicker = false }
            Button("Last 1 hour") { model.window.range(lower: 0, upper: 120); timePicker = false }
            Divider()
            DatePicker("From", selection: $from, in: model.date(0)...model.date(120))
            DatePicker("To", selection: $to, in: model.date(0)...model.date(120))
            Button("Apply range") {
                guard let snapshot = model.snapshot else { return }

                let lower = Int((from.timeIntervalSince1970 - Double(snapshot.start)) / Double(snapshot.step))
                let upper = Int((to.timeIntervalSince1970 - Double(snapshot.start)) / Double(snapshot.step))
                model.window.range(lower: min(120, max(0, lower)), upper: min(120, max(lower, upper)))
                timePicker = false
            }.disabled(from > to)
            Text("Bounds reflect the captured hour. This does not change the Mac's clock.")
                .font(.caption).foregroundStyle(theme.muted)
        }.padding(theme.contentPadding).frame(width: 390)
    }
}
