import Foundation

// Captured external observations are presentation evidence, never live recording truth.
struct ReferenceSnapshot: Sendable {
    let start: UInt64
    let end: UInt64
    let step: UInt64
    let capturedAt: UInt64
    let source: String
    let prototypeSHA256: String
    let apps: [String]
    let processApps: [String]
    let totals: [String: [Double?]]
    let series: [String: [ReferenceSeries]]
    let pages: [String: [ReferencePanel]]
    let hourPages: [String: [ReferencePanel]]
    let appPages: [String: [ReferencePanel]]
    let help: [String: [String]]
    let navigation: [[String]]
    let computer: [ReferenceSpecification]
    let selfCapture: ReferenceSelfCapture
    let selfPanels: [ReferencePanel]

    static func load(bundle: Bundle = .main) throws -> Self {
        guard let directory = bundle.resourceURL else {
            throw ReferenceDataError.invalid("The application has no resource directory.")
        }
        return try load(directory: directory)
    }

    static func load(directory: URL) throws -> Self {
        let capture: ReferenceCapture = try read("reference_snapshot", directory: directory)
        let catalog: ReferenceCatalog = try read("reference_catalog", directory: directory)
        let diagnostics: ReferenceSelfCapture = try read("reference_self", directory: directory)
        try capture.validate()
        try catalog.validate()
        try diagnostics.validate()
        return Self(start: capture.start, end: capture.end, step: capture.step,
            capturedAt: capture.capturedAt, source: capture.source,
            prototypeSHA256: capture.prototypeSHA256, apps: capture.apps,
            processApps: capture.processApps, totals: capture.totals, series: capture.series,
            pages: catalog.pages, hourPages: catalog.hourPages, appPages: catalog.appPages,
            help: catalog.help, navigation: catalog.navigation, computer: catalog.computer,
            selfCapture: diagnostics, selfPanels: catalog.selfPanels)
    }

    private static func read<T: Decodable>(_ name: String, directory: URL) throws -> T {
        let url = directory.appendingPathComponent(name).appendingPathExtension("json")
        let handle = try FileHandle(forReadingFrom: url)
        defer { handle.closeFile() }
        guard let bytes = try handle.read(upToCount: 8 * 1024 * 1024 + 1),
              !bytes.isEmpty, bytes.count <= 8 * 1024 * 1024 else {
            throw ReferenceDataError.invalid("Reference resource size exceeds its bound: " + name)
        }
        return try JSONDecoder().decode(T.self, from: bytes)
    }

    func value(_ key: String, at index: Int) -> Double? {
        guard index >= 0, index < 121, let row = series[key]?.first else { return nil }
        return row.values[index]
    }
}

struct ReferenceSeries: Codable, Sendable {
    let labels: [String: String]
    let app: String?
    let values: [Double?]
}

struct ReferenceLine: Codable, Sendable {
    var name: String
    var color: String
    var values: [Double?]
    var unit: String?
    var stack: Bool?
    var negative: Bool?
    var dashed: Bool?
    var axis: String?
    var fill: Double?
    var drawStyle: String?
    var precision: Int?

    init(name: String, color: String, values: [Double?], unit: String? = nil,
         stack: Bool? = nil, negative: Bool? = nil, dashed: Bool? = nil,
         axis: String? = nil, fill: Double? = nil, drawStyle: String? = nil,
         precision: Int? = nil) {
        self.name = name
        self.color = color
        self.values = values
        self.unit = unit
        self.stack = stack
        self.negative = negative
        self.dashed = dashed
        self.axis = axis
        self.fill = fill
        self.drawStyle = drawStyle
        self.precision = precision
    }
}

struct ReferencePanel: Codable, Identifiable, Sendable {
    var id: String
    var title: String
    var unit: String
    var lines: [ReferenceLine]
    var help: String?
    var stack: Bool?
    var minimum: Double?
    var maximum: Double?
    var kind: String?
    var caption: String?
    var gradient: Bool?
    var lineWidth: Double?
    var fill: Double?
    var precision: Int?
    var states: [String]?
    var quantities: [[String]]?
    var source: String?
    var section: String?
    var fold: String?
    var times: [Double]?
    var tableHeadings: [String]?
    var tableRows: [[String]]?

    init(id: String, title: String, unit: String = "", lines: [ReferenceLine] = [],
         help: String? = nil, stack: Bool? = nil, minimum: Double? = nil,
         maximum: Double? = nil, kind: String? = "chart", caption: String? = nil,
         gradient: Bool? = nil, lineWidth: Double? = nil, fill: Double? = nil,
         precision: Int? = nil, states: [String]? = nil, quantities: [[String]]? = nil,
         source: String? = nil, section: String? = nil, fold: String? = nil,
         times: [Double]? = nil, tableHeadings: [String]? = nil,
         tableRows: [[String]]? = nil) {
        self.id = id
        self.title = title
        self.unit = unit
        self.lines = lines
        self.help = help
        self.stack = stack
        self.minimum = minimum
        self.maximum = maximum
        self.kind = kind
        self.caption = caption
        self.gradient = gradient
        self.lineWidth = lineWidth
        self.fill = fill
        self.precision = precision
        self.states = states
        self.quantities = quantities
        self.source = source
        self.section = section
        self.fold = fold
        self.times = times
        self.tableHeadings = tableHeadings
        self.tableRows = tableRows
    }
}

struct ReferenceSpecification: Codable, Sendable {
    let name: String
    let value: String
}

struct ReferenceSelfCapture: Codable, Sendable {
    let source: String
    let duration: UInt64
    let wallTime: UInt64?
    let reports: [ReferenceReport]
    let minutes: [ReferenceMinute]
    let footprints: [ReferenceFootprint]
    let seals: [ReferenceSeal]
    let tracking: ReferenceTracking
    let drain: ReferenceDrain
    let provenance: [ReferenceProvenance]
    let buildCommit: String
    let buildBinaryHashes: String

    fileprivate func validate() throws {
        let started = tracking.found.addingReportingOverflow(tracking.missed)
        guard duration == 300, wallTime == nil, reports.count == 30,
              minutes.count == 5, footprints.count == 5, seals.count == 6,
              provenance.count == 2, !started.overflow, started.partialValue == tracking.started else {
            throw ReferenceDataError.invalid("Unexpected saved diagnostic capture dimensions.")
        }
        for (index, report) in reports.enumerated() {
            guard report.second == UInt64(index + 1) * 10, report.costs.count <= 32,
                  [report.cpu, report.late, report.maximum, report.powerMw].allSatisfy(\.isFinite),
                  report.costs.values.allSatisfy({ $0.isFinite && $0 >= 0 }),
                  [report.slips, report.processes, report.refused, report.gone, report.dropped]
                    .allSatisfy({ $0 <= 1_000_000_000 }) else {
                throw ReferenceDataError.invalid("Invalid saved diagnostic report.")
            }
        }
        for (index, minute) in minutes.enumerated() {
            guard minute.second == UInt64(index + 1) * 60, minute.elapsedNs > 0,
                  [minute.collector, minute.persister, minute.combinedPercent].allSatisfy(\.isFinite)
            else { throw ReferenceDataError.invalid("Invalid saved minute report.") }
        }
        for seal in seals {
            guard seal.end > seal.first, seal.loss <= 1_000_000_000,
                  seal.eventsLost <= 1_000_000_000 else {
                throw ReferenceDataError.invalid("Invalid saved seal publication range.")
            }
        }
        guard drain.seconds.isFinite, drain.seconds >= 0 else {
            throw ReferenceDataError.invalid("Invalid saved drain duration.")
        }
    }
}

struct ReferenceReport: Codable, Sendable {
    let second: UInt64
    let cpu: Double
    let late: Double
    let maximum: Double
    let slips: UInt64
    let processes: UInt64
    let refused: UInt64
    let gone: UInt64
    let dropped: UInt64
    let costs: [String: Double]
    let powerMw: Double
    enum CodingKeys: String, CodingKey {
        case second, cpu, late, maximum, slips, processes, refused, gone, dropped, costs
        case powerMw = "power_mw"
    }
}

struct ReferenceMinute: Codable, Sendable {
    let second: UInt64
    let collector: Double
    let persister: Double
    let combinedPercent: Double
    let elapsedNs: UInt64
    let collectorCpuNs: UInt64
    let persisterCpuNs: UInt64
    let combinedCpuNs: UInt64
    enum CodingKeys: String, CodingKey {
        case second, collector, persister
        case combinedPercent = "combined_percent", elapsedNs = "elapsed_ns"
        case collectorCpuNs = "collector_cpu_ns", persisterCpuNs = "persister_cpu_ns"
        case combinedCpuNs = "combined_cpu_ns"
    }
}

struct ReferenceFootprint: Codable, Sendable {
    let second: UInt64
    let collectorCpuNs: UInt64
    let persisterCpuNs: UInt64
    let currentFootprintBytes: UInt64
    let lifetimeFootprintBytesMax: UInt64
    enum CodingKeys: String, CodingKey {
        case second
        case collectorCpuNs = "collector_cpu_ns", persisterCpuNs = "persister_cpu_ns"
        case currentFootprintBytes = "current_footprint_bytes"
        case lifetimeFootprintBytesMax = "lifetime_footprint_bytes_max"
    }
}

struct ReferenceSeal: Codable, Sendable {
    let window: UInt64
    let loss: UInt64
    let eventsLost: UInt64
    let sealNs: UInt64
    let stagedBytes: UInt64
    let first: UInt64
    let end: UInt64
    enum CodingKeys: String, CodingKey {
        case window, loss, first, end
        case eventsLost = "events_lost", sealNs = "seal_ns", stagedBytes = "staged_bytes"
    }
}

struct ReferenceTracking: Codable, Sendable {
    let started: UInt64
    let found: UInt64
    let missed: UInt64
}

struct ReferenceDrain: Codable, Sendable {
    let seconds: Double
    let status: Int32
}

struct ReferenceProvenance: Codable, Sendable {
    let path: String
    let sha256: String
}

enum ReferenceDataError: Error, LocalizedError {
    case invalid(String)
    var errorDescription: String? {
        switch self {
        case .invalid(let message): message
        }
    }
}

private struct ReferenceCapture: Decodable {
    let start: UInt64
    let end: UInt64
    let step: UInt64
    let capturedAt: UInt64
    let source: String
    let prototypeSHA256: String
    let apps: [String]
    let processApps: [String]
    let totals: [String: [Double?]]
    let series: [String: [ReferenceSeries]]

    func validate() throws {
        guard step == 30, end > start, end - start == 3_600,
              capturedAt >= end, apps.count <= 1_024, processApps.count == 12,
              series.count == 95, totals.count == 6,
              prototypeSHA256 == "902254bcd038294d0afeef763cdab1d07bdedbfa9a5cf22ccd35e4ca708ac97e"
        else { throw ReferenceDataError.invalid("Unexpected v30 machine capture dimensions.") }
        var count = 0
        for rows in series.values {
            guard rows.count <= 1_024 else {
                throw ReferenceDataError.invalid("Too many series in a reference family.")
            }
            count += rows.count
            for row in rows {
                guard row.labels.count <= 32, row.app.map(apps.contains) ?? true else {
                    throw ReferenceDataError.invalid("Invalid captured series identity.")
                }
                try validateReferenceValues(row.values, count: 121)
            }
        }
        guard count == 1_156 else {
            throw ReferenceDataError.invalid("Unexpected captured series count.")
        }
        for values in totals.values { try validateReferenceValues(values, count: 121) }
    }
}

private struct ReferenceCatalog: Decodable {
    let pages: [String: [ReferencePanel]]
    let hourPages: [String: [ReferencePanel]]
    let appPages: [String: [ReferencePanel]]
    let help: [String: [String]]
    let navigation: [[String]]
    let computer: [ReferenceSpecification]
    let selfPanels: [ReferencePanel]

    func validate() throws {
        guard pages.count == 11, hourPages.count == 11, appPages.count == 12,
              navigation.count == 11, help.count <= 64, computer.count == 7,
              selfPanels.count == 6 else {
            throw ReferenceDataError.invalid("Unexpected reference panel catalog dimensions.")
        }
        for collection in [pages, hourPages, appPages] {
            for panels in collection.values {
                guard panels.count <= 100 else {
                    throw ReferenceDataError.invalid("Reference page exceeds its panel bound.")
                }
                for panel in panels {
                    try validate(panel, sampleCount: panel.times?.count ?? 121)
                }
            }
        }
        for panel in selfPanels { try validate(panel, sampleCount: panel.times?.count ?? 0) }
    }

    private func validate(_ panel: ReferencePanel, sampleCount: Int) throws {
        guard panel.lines.count <= 1_024 else {
            throw ReferenceDataError.invalid("Reference panel exceeds its series bound.")
        }
        for line in panel.lines {
            try validateReferenceValues(line.values, count: sampleCount)
            guard line.color.first == "#", line.color.count == 7 else {
                throw ReferenceDataError.invalid("Invalid reference series color.")
            }
        }
    }
}

private func validateReferenceValues(_ values: [Double?], count: Int) throws {
    guard count > 0, count <= 121, values.count == count,
          values.allSatisfy({ $0.map(\.isFinite) ?? true }) else {
        throw ReferenceDataError.invalid("Invalid reference series samples.")
    }
}
