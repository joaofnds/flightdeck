import Foundation

struct LiveAppCounter: Decodable, Equatable, Hashable, Sendable {
    let value: UInt64

    init(_ value: UInt64) { self.value = value }

    init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        let text = try container.decode(String.self)
        guard !text.isEmpty, text.utf8.count <= 20,
              text.utf8.allSatisfy({ (48...57).contains($0) }),
              text == "0" || text.first != "0", let value = UInt64(text) else {
            throw DecodingError.dataCorruptedError(in: container,
                debugDescription: "Expected an exact unsigned decimal counter")
        }

        self.value = value
    }
}

struct LiveAppIdentityWords: Decodable, Equatable, Hashable, Sendable {
    let first: UInt64
    let second: UInt64

    init(from decoder: any Decoder) throws {
        var container = try decoder.unkeyedContainer()
        first = try container.decode(LiveAppCounter.self).value
        second = try container.decode(LiveAppCounter.self).value
        guard container.isAtEnd else {
            throw DecodingError.dataCorruptedError(in: container,
                debugDescription: "Expected exactly two identity words")
        }
    }
}

struct LiveAppStamp: Decodable, Equatable, Hashable, Sendable {
    let boot: LiveAppIdentityWords
    let run: LiveAppIdentityWords
    let publication: LiveAppCounter
    let continuousNs: LiveAppCounter?
    let awakeNs: LiveAppCounter?
}

struct LiveAppObservation: Decodable, Equatable, Hashable, Sendable {
    let recording: LiveAppIdentityWords
    let stamp: LiveAppStamp
    let runTick: LiveAppCounter
    let wallNs: LiveAppCounter?
}

enum LiveAppKeyKind: String, Decodable, Hashable, Sendable {
    case bundle, executable
}

struct LiveAppKeyIdentity: Equatable, Hashable, Sendable {
    let kind: LiveAppKeyKind
    let bytes: Data
}

struct LiveAppKey: Decodable, Sendable {
    let kind: LiveAppKeyKind
    let bytes: Data
    let display: String
    var identity: LiveAppKeyIdentity { LiveAppKeyIdentity(kind: kind, bytes: bytes) }

    private enum CodingKeys: String, CodingKey { case kind, bytesHex, display }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decode(LiveAppKeyKind.self, forKey: .kind)
        bytes = try liveAppBytes(container.decode(String.self, forKey: .bytesHex))
        display = try liveAppText(container.decode(String.self, forKey: .display))
    }
}

struct LiveAppBytes: Decodable, Sendable {
    let bytes: Data
    let display: String
    private enum CodingKeys: String, CodingKey { case bytesHex, display }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        bytes = try liveAppBytes(container.decode(String.self, forKey: .bytesHex))
        display = try liveAppText(container.decode(String.self, forKey: .display))
    }
}

struct LiveAppMeasure: Decodable, Equatable, Sendable {
    let known: LiveAppCounter
    let measuredMembers: UInt32
    let unavailableMembers: UInt32
    let overflowed: Bool

    var knownValue: UInt64? {
        measuredMembers > 0 && !overflowed ? known.value : nil
    }
    var coverage: String {
        let suffix = overflowed ? " · overflow" : ""
        return "\(measuredMembers) measured · \(unavailableMembers) unavailable" + suffix
    }
}

enum LiveAppUnavailable: String, Decodable, Sendable {
    case refused, gone, unknown, interval, creation, baseline, overflow, exec, program
    case counterReset = "counter_reset"
    case exitUnverified = "exit_unverified"
}

enum LiveAppIdentityUnavailable: String, Decodable, Sendable {
    case refused, gone, unsupported, changed, invalid, exhausted
}

struct LiveAppAmount: Decodable, Equatable, Sendable {
    let known: LiveAppCounter?
    let unavailable: LiveAppUnavailable?
    private enum CodingKeys: String, CodingKey { case known, unavailable }

    init(known: UInt64?, unavailable: LiveAppUnavailable?) {
        precondition((known != nil) != (unavailable != nil))
        self.known = known.map(LiveAppCounter.init)
        self.unavailable = unavailable
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        known = try container.decodeIfPresent(LiveAppCounter.self, forKey: .known)
        unavailable = try container.decodeIfPresent(LiveAppUnavailable.self, forKey: .unavailable)
        guard (known != nil) != (unavailable != nil) else {
            throw LiveAppDataError.invalidQuantity
        }
    }
}

struct LiveAppRow: Decodable, Sendable {
    let appIndex: UInt32
    let key: LiveAppKey
    let displayName: String
    let members: UInt32
    let liveMembers: UInt32
    let membershipComplete: Bool
    let cpuNs: LiveAppMeasure
    let energyNj: LiveAppMeasure
    let residentBytes: LiveAppMeasure
    let footprintBytes: LiveAppMeasure
    let cpuPercentOneCore: Double?
    let energyJoules: Double?
    let energyWatts: Double?
}

struct LiveAppCreation: Decodable, Equatable, Sendable {
    let uniqueId: LiveAppCounter
    let pidVersion: Int32
}

enum LiveAppProcessState: String, Decodable, Sendable {
    case live
    case exitedVerified = "exited_verified"
    case exitedUnverified = "exited_unverified"
}

struct LiveAppProcessRowIdentity: Hashable, Sendable {
    let pid: Int32
    let uniqueId: UInt64?
}

struct LiveAppProcess: Decodable, Sendable {
    let pid: Int32
    let creation: LiveAppCreation?
    let appIndex: UInt32?
    let executableName: LiveAppBytes?
    let programUnavailable: LiveAppIdentityUnavailable?
    let cpuNs: LiveAppAmount
    let energyNj: LiveAppAmount
    let residentBytes: LiveAppAmount
    let footprintBytes: LiveAppAmount
    let appAttribution: LiveAppUnavailable?
    let state: LiveAppProcessState
    let cpuPercentOneCore: Double?
    let energyJoules: Double?
    let energyWatts: Double?

    var identity: LiveAppProcessRowIdentity {
        LiveAppProcessRowIdentity(pid: pid, uniqueId: creation?.uniqueId.value)
    }
}

struct LiveAppHeader: Decodable, Equatable, Sendable {
    let selected: LiveAppObservation
    let previous: LiveAppStamp?
    let elapsedNs: LiveAppCounter?
    let membershipComplete: Bool
    let ungroupedMembers: UInt32
    let usageRefused: UInt32
    let usageGone: UInt32
    let appCount: UInt32
    let processCount: UInt32

    func validate() throws {
        guard appCount <= 24_000, processCount <= 24_000,
              ungroupedMembers <= processCount else { throw LiveAppDataError.populationBound }
    }
}

struct LiveAppPage: Decodable, Sendable {
    let offset: UInt32
    let limit: UInt32
    let total: UInt32
    let nextOffset: UInt32?

    func validate(rows: Int) throws {
        guard limit > 0, limit <= 512, total <= 24_000, offset <= total,
              rows == Int(min(limit, total - offset)) else {
            throw LiveAppDataError.invalidPage
        }

        let end = offset + UInt32(rows)
        guard nextOffset == (end < total ? end : nil) else {
            throw LiveAppDataError.invalidPage
        }
    }
}

struct LiveAppListPage: Decodable, Sendable {
    let header: LiveAppHeader
    let apps: [LiveAppRow]
    let page: LiveAppPage
    private enum CodingKeys: String, CodingKey { case apps, page }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        header = try LiveAppHeader(from: decoder)
        apps = try liveAppRows(container.nestedUnkeyedContainer(forKey: .apps), type: LiveAppRow.self)
        page = try container.decode(LiveAppPage.self, forKey: .page)
        try header.validate()
        try page.validate(rows: apps.count)
        for app in apps { try app.validate() }
        guard page.total == header.appCount,
              Set(apps.map { $0.key.identity }).count == apps.count,
              apps.enumerated().allSatisfy({ $0.element.appIndex == page.offset + UInt32($0.offset) })
        else { throw LiveAppDataError.invalidPage }
    }
}

struct LiveAppDetailPage: Decodable, Sendable {
    let header: LiveAppHeader
    let app: LiveAppRow
    let processes: [LiveAppProcess]
    let page: LiveAppPage
    private enum CodingKeys: String, CodingKey { case app, processes, page }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        header = try LiveAppHeader(from: decoder)
        app = try container.decode(LiveAppRow.self, forKey: .app)
        processes = try liveAppRows(container.nestedUnkeyedContainer(forKey: .processes),
            type: LiveAppProcess.self)
        page = try container.decode(LiveAppPage.self, forKey: .page)
        try header.validate()
        try page.validate(rows: processes.count)
        try app.validate()
        guard app.appIndex < header.appCount, page.total == app.members,
              Set(processes.map(\.identity)).count == processes.count,
              processes.allSatisfy({ $0.pid > 0 && $0.appIndex == app.appIndex }) else {
            throw LiveAppDataError.invalidPage
        }
    }
}

enum LiveAppDataError: Error, Equatable {
    case invalidBytes, invalidText, invalidQuantity, populationBound, invalidPage
}

private func liveAppBytes(_ text: String) throws -> Data {
    guard !text.isEmpty, text.utf8.count <= 8190, text.utf8.count.isMultiple(of: 2) else {
        throw LiveAppDataError.invalidBytes
    }

    let encoded = Array(text.utf8)
    var bytes = Data(capacity: encoded.count / 2)
    for index in stride(from: 0, to: encoded.count, by: 2) {
        guard let high = liveAppHexDigit(encoded[index]),
              let low = liveAppHexDigit(encoded[index + 1]) else {
            throw LiveAppDataError.invalidBytes
        }

        bytes.append(high * 16 + low)
    }

    return bytes
}

private func liveAppHexDigit(_ byte: UInt8) -> UInt8? {
    switch byte {
    case 48...57: byte - 48
    case 97...102: byte - 87
    default: nil
    }
}

private func liveAppText(_ text: String) throws -> String {
    guard text.utf8.count <= 16_380 else { throw LiveAppDataError.invalidText }

    return text
}

private func liveAppRows<Row: Decodable>(
    _ source: UnkeyedDecodingContainer, type: Row.Type
) throws -> [Row] {
    var source = source
    var result: [Row] = []
    result.reserveCapacity(min(512, source.count ?? 512))
    while !source.isAtEnd {
        guard result.count < 512 else { throw LiveAppDataError.invalidPage }

        result.append(try source.decode(type))
    }

    return result
}

extension LiveAppRow {
    fileprivate func validate() throws {
        guard members > 0, members <= 24_000, liveMembers <= members else {
            throw LiveAppDataError.populationBound
        }

        guard displayName.utf8.count <= 16_380 else { throw LiveAppDataError.invalidText }

        try cpuNs.validate(members: members)
        try energyNj.validate(members: members)
        try residentBytes.validate(members: liveMembers)
        try footprintBytes.validate(members: liveMembers)
        if cpuNs.knownValue == nil, cpuPercentOneCore != nil {
            throw LiveAppDataError.invalidQuantity
        }
        if energyNj.knownValue == nil, energyJoules != nil || energyWatts != nil {
            throw LiveAppDataError.invalidQuantity
        }
    }
}

extension LiveAppMeasure {
    fileprivate func validate(members: UInt32) throws {
        guard UInt64(measuredMembers) + UInt64(unavailableMembers) == UInt64(members) else {
            throw LiveAppDataError.invalidQuantity
        }

        if measuredMembers == 0, known.value != 0 { throw LiveAppDataError.invalidQuantity }
    }
}
