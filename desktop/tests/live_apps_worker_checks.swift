import Foundation

@MainActor
func reads_live_apps_through_the_real_worker(path: String) throws {
    // Goal: live App DTOs and native plots retain exact source identity across following and pinning.
    // Method: read a real registered v4 source with two measured ticks and one absent-App tick.
    let harness = LiveAppsWorkerHarness(path: path)
    let latest = try harness.driver.follow(poll: false)
    try checks_absent_app_with_retained_history(latest)
    guard let reference = latest.catalog.latestObservation else {
        throw RecordingError(message: "Native App fixture has no latest source observation")
    }

    let pinned = try harness.driver.pin(publication: 2, second: 1001, reference: reference)
    try checks_pinned_app_quantities(pinned)
    try checks_exact_app_history(pinned)
    let otherKey = LiveAppKeyIdentity(kind: .bundle, bytes: Data("/Applications/Other.app".utf8))
    let other = try harness.driver.pin(publication: 2, second: 1001,
        reference: reference, key: otherKey)
    try checks_same_observation_with_another_key(other, key: otherKey)
    let restored = try harness.driver.pin(publication: 2, second: 1001, reference: reference)
    try checks_pinned_app_quantities(restored)
    let unchanged = try harness.driver.pin(publication: 2, second: 1001, reference: reference)
    try liveAppsWorkerRequire(unchanged.unchanged, "An identical projection repeated its full query")
    try checks_pinned_app_quantities(unchanged)

    let resumed = try harness.driver.follow(poll: true)
    try checks_absent_app_with_retained_history(resumed)
    try liveAppsWorkerRequire(resumed.selection?.selected?.publication == 3,
        "Following after pin retained publication \(String(describing: resumed.selection?.selected?.publication))")
    print("Real RecordingWorker App totals: 50% of one core, 3 J, 3 W, 150 MiB")
    print("Real RecordingWorker App identities, exact history, pin/follow and absence checks passed")
}

@MainActor
func reads_live_apps_paging_through_the_real_worker(path: String) throws {
    // Goal: following and manual selection recover when retained pages exceed a smaller population.
    // Method: pin page 512 of 513 Apps/members, then request the 511-row observation with those offsets.
    let harness = LiveAppsWorkerHarness(path: path)
    let latest = try harness.driver.follow(poll: false)
    let reference = try liveAppsWorkerValue(latest.catalog.latestObservation,
        "paging fixture has no current observation")
    let large = try harness.driver.pin(publication: 1, second: 1000, reference: reference,
        appsOffset: 512, membersOffset: 512)
    try checks_population_page(large, publication: 1, count: 513, offset: 512, rows: 1)

    let followed = try harness.driver.follow(poll: true, appsOffset: 512, membersOffset: 512)
    try checks_population_page(followed, publication: 2, count: 511, offset: 0, rows: 511)
    let largeAgain = try harness.driver.pin(publication: 1, second: 1000, reference: reference,
        appsOffset: 512, membersOffset: 512)
    try checks_population_page(largeAgain, publication: 1, count: 513, offset: 512, rows: 1)
    let smaller = try harness.driver.pin(publication: 2, second: 1001, reference: reference,
        appsOffset: 512, membersOffset: 512)
    try checks_population_page(smaller, publication: 2, count: 511, offset: 0, rows: 511)
    print("Real RecordingWorker App/member paging: 513 to 511 resets offsets 512 to 0")
}

private func checks_population_page(_ response: ReadResult, publication: UInt64,
                                    count: UInt32, offset: UInt32, rows: Int) throws {
    let apps = try liveAppsWorkerValue(response.apps, "missing population projection")
    let detail = try liveAppsWorkerValue(apps.detail, "selected raw App key disappeared")

    try liveAppsWorkerRequire(response.selection?.selected?.publication == publication,
        "Paging did not select expected publication \(publication)")
    try liveAppsWorkerRequire(apps.key == LiveAppsWorkerDriver.key,
        "Paging changed the selected raw App key bytes")
    try liveAppsWorkerRequire(detail.app.key.identity == LiveAppsWorkerDriver.key,
        "Paging returned another App's detail")
    try liveAppsWorkerRequire(apps.list.header.appCount == count && apps.list.page.total == count,
        "Paging did not retain the current App population")
    try liveAppsWorkerRequire(detail.app.members == count && detail.page.total == count,
        "Paging did not retain the selected App's current members")
    try liveAppsWorkerRequire(apps.list.page.offset == offset && detail.page.offset == offset,
        "Paging retained out-of-range offsets instead of returning effective page \(offset)")
    try liveAppsWorkerRequire(apps.list.apps.count == rows && detail.processes.count == rows,
        "Paging returned an empty or truncated current page")
    try liveAppsWorkerRequire(detail.header.selected == apps.list.header.selected,
        "App list and member pages mixed source observations")
}

private func checks_same_observation_with_another_key(_ response: ReadResult,
                                                      key: LiveAppKeyIdentity) throws {
    let apps = try liveAppsWorkerValue(response.apps, "missing App projection response")
    let history = try liveAppsWorkerValue(apps.history, "missing other-key history")

    try liveAppsWorkerRequire(response.selection?.selected?.publication == 2,
        "Changing the App key changed the selected publication")
    try liveAppsWorkerRequire(apps.key == key, "Unchanged-source cache retained the previous App key")
    try liveAppsWorkerRequire(!response.unchanged, "Changing an App key skipped its new projection")
    try liveAppsWorkerRequire(apps.detail == nil, "Another App key inherited the old process detail")
    try liveAppsWorkerRequire(history.points.count == 2, "Another App key lost observation coverage")
    try liveAppsWorkerRequire(history.points.allSatisfy { $0.members == nil },
        "Another App key inherited the old member history")
    try liveAppsWorkerRequire(history.points.allSatisfy { $0.cpuNs == nil },
        "Another App key inherited the old CPU history")
}

private func checks_absent_app_with_retained_history(_ response: ReadResult) throws {
    let apps = try liveAppsWorkerValue(response.apps, "missing App response")
    let history = try liveAppsWorkerValue(apps.history, "missing retained App history")

    try liveAppsWorkerRequire(response.selection?.selected?.publication == 3,
        "Absent-App follow selected \(String(describing: response.selection?.selected?.publication)), expected 3")
    try liveAppsWorkerRequire(response.catalog.latestObservation?.publication == 3,
        "Catalog did not retain latest publication 3")
    try liveAppsWorkerRequire(apps.list.header.selected.stamp.publication.value == 3,
        "App list did not identify publication 3")
    try liveAppsWorkerRequire(apps.list.apps.isEmpty, "Absent App still appeared in current App list")
    try liveAppsWorkerRequire(apps.detail == nil, "Absent App retained a current process detail")
    try liveAppsWorkerRequire(apps.notice != nil, "Absent App had no availability explanation")
    try liveAppsWorkerRequire(history.selected == apps.list.header.selected,
        "History and selected App response identified different observations")
    try liveAppsWorkerRequire(history.points.map { $0.observation.stamp.publication.value } == [1, 2, 3],
        "Retained App history did not contain the three source publications")
    try liveAppsWorkerRequire(history.points[1].cpuPercentOneCore == 50,
        "Absence discarded the measured 50% CPU history")
    try liveAppsWorkerRequire(history.points[2].members == nil,
        "Absent App acquired a manufactured member count")
    try liveAppsWorkerRequire(history.points[2].residentBytes == nil,
        "Absent App acquired a manufactured resident gauge")
    try liveAppsWorkerRequire(history.points[2].cpuNs == nil,
        "Absent App acquired a manufactured CPU subtotal")
    try liveAppsWorkerRequire(history.coverage.observationsRetained == 3,
        "Unexpected retained App observation count")
    try liveAppsWorkerRequire(!history.coverage.retentionLimited,
        "Tiny App fixture unexpectedly shortened its history")
    try liveAppsWorkerRequire(history.coverage.cacheBytes <= history.coverage.cacheBytesMax,
        "App history exceeded its declared memory bound")
}

private func checks_pinned_app_quantities(_ response: ReadResult) throws {
    let apps = try liveAppsWorkerValue(response.apps, "missing pinned App response")
    let detail = try liveAppsWorkerValue(apps.detail, "missing pinned App detail")
    let key = LiveAppsWorkerDriver.key

    try liveAppsWorkerRequire(response.catalog.latestObservation?.publication == 3,
        "Pinning changed the global latest observation")
    try liveAppsWorkerRequire(response.selection?.selected?.publication == 2,
        "Pinned worker did not select publication 2")
    try liveAppsWorkerRequire(detail.header.selected.stamp.publication.value == 2,
        "Pinned App detail used another publication")
    try liveAppsWorkerRequire(detail.header.selected == apps.list.header.selected,
        "Pinned App list and processes do not share the exact observation")
    try liveAppsWorkerRequire(detail.app.key.identity == key, "Captured App key bytes changed")
    try liveAppsWorkerRequire(detail.app.cpuNs.known.value == 500_000_000,
        "App CPU delta is not the exact 500,000,000 ns subtotal")
    try liveAppsWorkerRequire(detail.app.cpuPercentOneCore == 50, "App CPU is not 50% of one core")
    try liveAppsWorkerRequire(detail.app.energyNj.known.value == 3_000_000_000,
        "App energy delta is not the exact 3,000,000,000 nJ subtotal")
    try liveAppsWorkerRequire(detail.app.energyJoules == 3, "App interval energy is not 3 J")
    try liveAppsWorkerRequire(detail.app.energyWatts == 3, "App interval power is not 3 W")
    try liveAppsWorkerRequire(detail.app.residentBytes.known.value == 157_286_400,
        "App resident memory is not 150 MiB")
    try liveAppsWorkerRequire(detail.app.footprintBytes.known.value == 165_000_000,
        "App footprint is not its separate captured 165,000,000-byte gauge")
    try liveAppsWorkerRequire(detail.header.elapsedNs?.value == 1_000_000_000,
        "App interval did not preserve one continuous elapsed second")
    try liveAppsWorkerRequire(!detail.app.membershipComplete, "App claimed complete event membership")
    try liveAppsWorkerRequire(detail.app.cpuNs.measuredMembers == 2,
        "App CPU coverage did not preserve two measured members")
    try liveAppsWorkerRequire(detail.app.cpuNs.unavailableMembers == 0,
        "App CPU coverage manufactured an unavailable member")
    try liveAppsWorkerRequire(detail.processes.map(\.pid) == [42, 43], "App process PIDs changed")
    try liveAppsWorkerRequire(detail.processes.map { $0.creation?.uniqueId.value }
        == [9_007_199_254_740_993, 9_007_199_254_740_994], "Wide process creation identities changed")
    try liveAppsWorkerRequire(detail.processes.map { $0.creation?.pidVersion } == [-70, 71],
        "Signed process PID versions changed")
    try liveAppsWorkerRequire(detail.processes.map { $0.cpuNs.known?.value }
        == [200_000_000, 300_000_000], "Individual process CPU deltas changed")
    try liveAppsWorkerRequire(detail.processes.map { $0.energyNj.known?.value }
        == [1_000_000_000, 2_000_000_000], "Individual process energy deltas changed")
}

@MainActor
private func checks_exact_app_history(_ response: ReadResult) throws {
    let apps = try liveAppsWorkerValue(response.apps, "missing App history response")
    let history = try liveAppsWorkerValue(apps.history, "missing pinned App history")
    let selected = try liveAppsWorkerValue(response.selection?.selected, "missing pinned observation")
    let expected = ["\(selected.boot):\(selected.run):1", "\(selected.boot):\(selected.run):2"]
    let view = try liveAppsWorkerValue(history.view(key: LiveAppsWorkerDriver.key,
        theme: FlightdeckTheme()), "App chart projection refused valid history")
    let cpu = try liveAppsWorkerValue(view.cpu, "missing CPU chart")
    let resident = try liveAppsWorkerValue(view.resident, "missing resident chart")
    let power = try liveAppsWorkerValue(view.power, "missing power chart")

    try liveAppsWorkerRequire(history.contextSeconds == 900, "App history did not follow 15 minutes")
    try liveAppsWorkerRequire(history.selected == apps.list.header.selected,
        "Pinned App history used a different selected identity")
    try liveAppsWorkerRequire(history.points.map(\.id) == expected, "App history IDs changed")
    try liveAppsWorkerRequire(cpu.samples.map(\.id) == expected, "Native CPU plot IDs changed")
    try liveAppsWorkerRequire(cpu.lines[0].values == [nil, 50], "CPU chart invented an initial baseline")
    try liveAppsWorkerRequire(resident.lines[0].values == [0.1171875, 0.146484375],
        "Resident chart did not retain 120 MiB then 150 MiB")
    try liveAppsWorkerRequire(power.lines[0].values == [nil, 3], "Power chart changed exact intervals")
    try liveAppsWorkerRequire(view.selectedID == expected[1], "Chart readout did not target publication 2")
    try liveAppsWorkerRequire(cpu.domain.upperBound - cpu.domain.lowerBound == 900,
        "App chart did not retain a 900-second domain")
    try liveAppsWorkerRequire(history.points.allSatisfy { !$0.membershipComplete },
        "App history claimed complete event membership")
}

private struct LiveAppsWorkerHarness {
    let driver: LiveAppsWorkerDriver
    init(path: String) { driver = LiveAppsWorkerDriver(path: path) }
}

private final class LiveAppsWorkerDriver {
    static let key = LiveAppKeyIdentity(kind: .bundle,
        bytes: Data("/Applications/Example".utf8) + Data([255]) + Data(".app".utf8))
    private let worker = RecordingWorker()
    private let path: String

    init(path: String) { self.path = path }

    func follow(poll: Bool, appsOffset: UInt32 = 0, membersOffset: UInt32 = 0) throws -> ReadResult {
        try read(second: nil, target: nil, poll: poll, key: Self.key,
            appsOffset: appsOffset, membersOffset: membersOffset)
    }

    func pin(publication: UInt64, second: UInt64, reference: Observation,
             key: LiveAppKeyIdentity = LiveAppsWorkerDriver.key,
             appsOffset: UInt32 = 0, membersOffset: UInt32 = 0) throws -> ReadResult {
        let identity = RecordedObservationIdentity(run: reference.run, boot: reference.boot,
            publication: publication)

        return try read(second: second, target: identity, poll: true, key: key,
            appsOffset: appsOffset, membersOffset: membersOffset)
    }

    private func read(second: UInt64?, target: RecordedObservationIdentity?, poll: Bool,
                      key: LiveAppKeyIdentity, appsOffset: UInt32,
                      membersOffset: UInt32) throws -> ReadResult {
        let completion = LiveAppsWorkerCompletion()
        worker.read(ReadRequest(path: path, second: second, alternative: 0, table: 1, offset: 0,
            refresh: false, machineOffsets: [:], includeMachine: false, live: true, poll: poll,
            target: target, apps: LiveAppsRequest(key: key, appsOffset: appsOffset, membersOffset: membersOffset))) {
            completion.complete($0)
        }
        guard completion.ready.wait(timeout: .now() + 5) == .success else {
            throw RecordingError(message: "Real App worker exceeded five seconds")
        }
        guard let result = completion.take() else {
            throw RecordingError(message: "Real App worker returned no result")
        }

        let response = try result.get()
        try liveAppsWorkerRequire(response.page == nil,
            "An App projection also queried the unused raw table page")

        return response
    }
}

private final class LiveAppsWorkerCompletion: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<ReadResult, RecordingError>?
    let ready = DispatchSemaphore(value: 0)

    func complete(_ value: Result<ReadResult, RecordingError>) {
        lock.withLock { result = value }
        ready.signal()
    }

    func take() -> Result<ReadResult, RecordingError>? { lock.withLock { result } }
}

private func liveAppsWorkerRequire(_ condition: Bool, _ message: String) throws {
    guard condition else { throw RecordingError(message: "Native App fixture: " + message) }
}

private func liveAppsWorkerValue<Value>(_ value: Value?, _ message: String) throws -> Value {
    guard let value else { throw RecordingError(message: "Native App fixture: " + message) }

    return value
}
