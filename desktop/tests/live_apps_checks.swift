import AppKit
import Foundation
import SwiftUI

@MainActor
func runLiveAppsChecks() throws {
    try preserves_exact_live_app_counters_above_double_precision()
    try groups_captured_executables_with_exact_subtotals_and_coverage()
    try keeps_reversible_byte_keys_distinct_from_display_text()
    refuses_numeric_json_for_an_exact_counter()
    refuses_decimal_counter_overflow()
    try preserves_the_maximum_unsigned_counter()
    try keeps_unavailable_quantities_separate_from_measured_zero()
    refuses_conflicting_quantity_states()
    refuses_a_partial_hex_byte()
    try preserves_signed_pid_versions_from_captured_identity()
    try labels_group_subtotals_as_a_partial_loaded_page()
    refuses_a_member_from_another_app_index()
    try round_trips_generated_exact_counters()
    preserves_exact_history_selection_when_wall_times_repeat()
    try renders_exact_process_identities_and_readings_in_fixed_native_rows()
    try renders_group_subtotal_coverage_in_native_cells()
    try distinguishes_native_measured_zero_from_absent_identity_and_readings()
    try activates_only_known_group_buttons_without_update_callbacks()
    refreshes_reused_native_cells_without_stale_text_or_help()
    try applies_native_live_table_theme_without_enabling_row_selection()
    renders_overflowed_group_subtotals_as_unavailable()
    try renders_the_loaded_process_bound_and_clears_an_empty_page()
    print("22 live App native checks passed")
}

private func preserves_exact_live_app_counters_above_double_precision() throws {
    // Goal: native decoding preserves a captured counter beyond Double's exact integer range.
    // Method: decode the independently specified odd integer as a decimal string.
    let data = Data("\"9007199254740993\"".utf8)

    let counter = try JSONDecoder().decode(LiveAppCounter.self, from: data)

    precondition(counter.value == 9_007_199_254_740_993)
}

enum LiveAppsFixture {
    static func read(otherApp: Bool = false) throws -> LiveAppsRead {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let text = String(decoding: detail, as: UTF8.self)
        let bytes = Data((otherApp ? text.replacingOccurrences(of: "2f412e617070", with: "2f422e617070")
            .replacingOccurrences(of: "A.app", with: "B.app") : text).utf8)
        let detail = try decoder.decode(LiveAppDetailPage.self, from: bytes)
        guard var object = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              let app = object["app"], let selected = object["selected"] else {
            throw LiveAppDataError.invalidPage
        }

        object["apps"] = [app]
        object["page"] = ["offset": 0, "limit": 512, "total": 1, "next_offset": NSNull()]
        let list = try decoder.decode(LiveAppListPage.self,
            from: JSONSerialization.data(withJSONObject: object))
        let history: [String: Any] = ["selected": selected, "context_seconds": 900,
            "points": [["observation": selected, "segment": "0", "elapsed_ns": "1000000000",
                "members": 3, "membership_complete": false,
                "cpu_ns": ["known": "500000000", "measured_members": 2,
                    "unavailable_members": 1, "overflowed": false],
                "energy_nj": ["known": "3000000000", "measured_members": 2,
                    "unavailable_members": 1, "overflowed": false],
                "resident_bytes": ["known": "157286400", "measured_members": 2,
                    "unavailable_members": 1, "overflowed": false],
                "footprint_bytes": ["known": "165000000", "measured_members": 2,
                    "unavailable_members": 1, "overflowed": false],
                "cpu_percent_one_core": 50, "energy_joules": 3, "energy_watts": 3]],
            "coverage": ["retention_limited": false, "observations_retained": 1,
                "cache_bytes": 4096, "cache_bytes_max": 33_554_432]]

        return LiveAppsRead(key: detail.app.key.identity, list: list, detail: detail,
            history: try decoder.decode(LiveAppHistoryPayload.self,
                from: JSONSerialization.data(withJSONObject: history)), notice: nil)
    }

    static let detail = Data(#"""
    {
      "selected":{"recording":["1","2"],"stamp":{"boot":["3","4"],"run":["5","6"],
        "publication":"9007199254740993","continuous_ns":"2000000000","awake_ns":"2000000000"},
        "run_tick":"2","wall_ns":"1800000000000000000"},
      "previous":{"boot":["3","4"],"run":["5","6"],"publication":"9007199254740992",
        "continuous_ns":"1000000000","awake_ns":"1000000000"},
      "elapsed_ns":"1000000000","membership_complete":false,"ungrouped_members":0,
      "usage_refused":1,"usage_gone":0,"app_count":1,"process_count":3,
      "app":{"app_index":0,"key":{"kind":"bundle","bytes_hex":"2f412e617070","display":"/A.app"},
        "display_name":"A.app","members":3,"live_members":3,"membership_complete":false,
        "cpu_ns":{"known":"500000000","measured_members":2,"unavailable_members":1,"overflowed":false},
        "energy_nj":{"known":"3000000000","measured_members":2,"unavailable_members":1,"overflowed":false},
        "resident_bytes":{"known":"157286400","measured_members":2,"unavailable_members":1,"overflowed":false},
        "footprint_bytes":{"known":"165000000","measured_members":2,"unavailable_members":1,"overflowed":false},
        "cpu_percent_one_core":50,"energy_joules":3,"energy_watts":3},
      "processes":[
        {"pid":42,"creation":{"unique_id":"9007199254740993","pid_version":1},"app_index":0,
         "executable_name":{"bytes_hex":"68656c706572ff","display":"helper\\xff"},
         "program_unavailable":null,"cpu_ns":{"known":"200000000","unavailable":null},
         "energy_nj":{"known":"1000000000","unavailable":null},
         "resident_bytes":{"known":"104857600","unavailable":null},
         "footprint_bytes":{"known":"110000000","unavailable":null},
         "app_attribution":null,"state":"live","cpu_percent_one_core":20,"energy_joules":1,"energy_watts":1},
        {"pid":43,"creation":{"unique_id":"9007199254740994","pid_version":1},"app_index":0,
         "executable_name":{"bytes_hex":"68656c706572ff","display":"helper\\xff"},
         "program_unavailable":null,"cpu_ns":{"known":"300000000","unavailable":null},
         "energy_nj":{"known":"2000000000","unavailable":null},
         "resident_bytes":{"known":"52428800","unavailable":null},
         "footprint_bytes":{"known":"55000000","unavailable":null},
         "app_attribution":null,"state":"live","cpu_percent_one_core":30,"energy_joules":2,"energy_watts":2},
        {"pid":44,"creation":{"unique_id":"9007199254740995","pid_version":1},"app_index":0,
         "executable_name":{"bytes_hex":"68656c706572ff","display":"helper\\xff"},
         "program_unavailable":null,"cpu_ns":{"known":null,"unavailable":"refused"},
         "energy_nj":{"known":null,"unavailable":"refused"},
         "resident_bytes":{"known":null,"unavailable":"unknown"},
         "footprint_bytes":{"known":null,"unavailable":"unknown"},
         "app_attribution":null,"state":"live","cpu_percent_one_core":null,"energy_joules":null,"energy_watts":null}
      ],"page":{"offset":0,"limit":512,"total":3,"next_offset":null}
    }
    """#.utf8)
}

private func groups_captured_executables_with_exact_subtotals_and_coverage() throws {
    // Goal: executable grouping preserves known quantities and an unavailable third member.
    // Method: decode independent 20%/30%, 100/50 MiB and 1/2 J process literals.
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let detail = try decoder.decode(LiveAppDetailPage.self, from: LiveAppsFixture.detail)

    let groups = LiveAppGroups(detail: detail)

    precondition(groups.rows.count == 1)
    let group = groups.rows[0]
    precondition(group.instances == 3)
    precondition(group.cpuNs.known.value == 500_000_000)
    precondition(group.residentBytes.known.value == 157_286_400)
    precondition(group.energyNj.known.value == 3_000_000_000)
    precondition(group.cpuNs.measuredMembers == 2)
    precondition(group.cpuNs.unavailableMembers == 1)
    precondition(group.cpuPercentOneCore == 50,
        "CPU=\(String(describing: group.cpuPercentOneCore)) elapsed=\(String(describing: detail.header.elapsedNs))")
    precondition(group.energyWatts == 3)
}

private func keeps_reversible_byte_keys_distinct_from_display_text() throws {
    // Goal: native App identity cannot collapse distinct captured bytes sharing a display label.
    // Method: decode an invalid UTF-8 basename and a literal escape as two separate keys.
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let invalid = Data(#"{"kind":"executable","bytes_hex":"6e616d65ff","display":"same"}"#.utf8)
    let literal = Data(#"{"kind":"executable","bytes_hex":"6e616d655c786666","display":"same"}"#.utf8)

    let first = try decoder.decode(LiveAppKey.self, from: invalid)
    let second = try decoder.decode(LiveAppKey.self, from: literal)

    precondition(first.display == second.display)
    precondition(first.identity != second.identity)
    precondition(first.bytes == Data([110, 97, 109, 101, 255]))
    precondition(second.bytes == Data([110, 97, 109, 101, 92, 120, 102, 102]))
}

private func refuses_numeric_json_for_an_exact_counter() {
    // Goal: a numeric JSON token cannot silently enter the exact string-counter boundary.
    // Method: submit the wide odd integer as a number rather than its required string.
    let data = Data("9007199254740993".utf8)

    let result = Result { try JSONDecoder().decode(LiveAppCounter.self, from: data) }

    precondition(isDecodingFailure(result))
}

private func refuses_decimal_counter_overflow() {
    // Goal: an unsigned value larger than u64 remains unavailable rather than wrapping.
    // Method: decode one above the independently specified UInt64 maximum.
    let data = Data("\"18446744073709551616\"".utf8)

    let result = Result { try JSONDecoder().decode(LiveAppCounter.self, from: data) }

    precondition(isDecodingFailure(result))
}

private func preserves_the_maximum_unsigned_counter() throws {
    // Goal: exact counter decoding accepts its upper boundary without floating conversion.
    // Method: decode the independent decimal representation of the u64 maximum.
    let data = Data("\"18446744073709551615\"".utf8)

    let result = try JSONDecoder().decode(LiveAppCounter.self, from: data)

    precondition(result.value == UInt64.max)
}

private func keeps_unavailable_quantities_separate_from_measured_zero() throws {
    // Goal: refusal stays unavailable while a measured zero stays measurable.
    // Method: decode independently specified zero and refused quantity states.
    let zero = Data(#"{"known":"0","unavailable":null}"#.utf8)
    let refused = Data(#"{"known":null,"unavailable":"refused"}"#.utf8)

    let measured = try JSONDecoder().decode(LiveAppAmount.self, from: zero)
    let missing = try JSONDecoder().decode(LiveAppAmount.self, from: refused)

    precondition(measured.known?.value == 0)
    precondition(measured.unavailable == nil)
    precondition(missing.known == nil)
    precondition(missing.unavailable == .refused)
}

private func refuses_conflicting_quantity_states() {
    // Goal: a quantity cannot simultaneously be known and refused.
    // Method: decode a malformed quantity through the same production Decodable type.
    let data = Data(#"{"known":"0","unavailable":"refused"}"#.utf8)

    let result = Result { try JSONDecoder().decode(LiveAppAmount.self, from: data) }

    precondition(isLiveAppFailure(result, expected: .invalidQuantity))
}

private func refuses_a_partial_hex_byte() {
    // Goal: malformed captured hex cannot create a truncated or guessed key.
    // Method: submit an odd-length raw key to the production decoder.
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let data = Data(#"{"kind":"executable","bytes_hex":"6e6","display":"n"}"#.utf8)

    let result = Result { try decoder.decode(LiveAppKey.self, from: data) }

    precondition(isLiveAppFailure(result, expected: .invalidBytes))
}

private func isDecodingFailure<Value>(_ result: Result<Value, Error>) -> Bool {
    switch result {
    case .success: false
    case .failure(let error): error is DecodingError
    }
}

private func isLiveAppFailure<Value>(
    _ result: Result<Value, Error>, expected: LiveAppDataError
) -> Bool {
    switch result {
    case .success: false
    case .failure(let error):
        switch error {
        case LiveAppDataError.invalidBytes: expected == .invalidBytes
        case LiveAppDataError.invalidText: expected == .invalidText
        case LiveAppDataError.invalidQuantity: expected == .invalidQuantity
        case LiveAppDataError.populationBound: expected == .populationBound
        case LiveAppDataError.invalidPage: expected == .invalidPage
        default: false
        }
    }
}

private func preserves_signed_pid_versions_from_captured_identity() throws {
    // Goal: native identity preserves the source's signed i32 PID version.
    // Method: decode a valid captured negative version from the retained v4 contract.
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let data = Data(#"{"unique_id":"9007199254740993","pid_version":-70}"#.utf8)

    let creation = try decoder.decode(LiveAppCreation.self, from: data)

    precondition(Int64(creation.pidVersion) == -70)
}

private func labels_group_subtotals_as_a_partial_loaded_page() throws {
    // Goal: three loaded members cannot look like the complete 513-member App population.
    // Method: decode a valid first page with an independently larger App/member total.
    let text = String(decoding: LiveAppsFixture.detail, as: UTF8.self)
        .replacingOccurrences(of: "\"process_count\":3", with: "\"process_count\":513")
        .replacingOccurrences(of: "\"members\":3", with: "\"members\":513")
        .replacingOccurrences(of: "\"live_members\":3", with: "\"live_members\":513")
        .replacingOccurrences(of: "\"unavailable_members\":1", with: "\"unavailable_members\":511")
        .replacingOccurrences(of: "\"limit\":512", with: "\"limit\":3")
        .replacingOccurrences(of: "\"total\":3", with: "\"total\":513")
        .replacingOccurrences(of: "\"next_offset\":null", with: "\"next_offset\":3")
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let detail = try decoder.decode(LiveAppDetailPage.self, from: Data(text.utf8))

    let groups = LiveAppGroups(detail: detail)

    precondition(groups.loadedMembers == 3)
    precondition(groups.totalMembers == 513)
    precondition(groups.coverage == "3 of 513 processes loaded · group subtotals cover this page")
    precondition(groups.rows[0].cpuPercentOneCore == 50)
}

private func refuses_a_member_from_another_app_index() {
    // Goal: a detail page cannot attribute another App's process to the selected App.
    // Method: change only process app_index fields while retaining the valid enclosing App.
    let text = String(decoding: LiveAppsFixture.detail, as: UTF8.self)
        .replacingOccurrences(of: "\"app_index\":0,\n     \"executable_name\"",
            with: "\"app_index\":1,\n     \"executable_name\"")
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase

    let result = Result { try decoder.decode(LiveAppDetailPage.self, from: Data(text.utf8)) }

    precondition(isLiveAppFailure(result, expected: .invalidPage))
}

private func round_trips_generated_exact_counters() throws {
    // Goal: native exact decoding preserves arbitrary counters throughout the u64 range.
    // Method: decode 128 independently generated decimal strings through the production type.
    var seed: UInt64 = 0x8242_03ec
    for index in 0..<128 {
        seed = seed &* 6_364_136_223_846_793_005 &+ 1
        let data = try JSONEncoder().encode(String(seed))

        let counter = try JSONDecoder().decode(LiveAppCounter.self, from: data)

        precondition(counter.value == seed, "Generated counter \(index): \(seed)")
    }
}

private func preserves_exact_history_selection_when_wall_times_repeat() {
    // Goal: repeated wall time cannot make the selected App readout identify another publication.
    // Method: select the second of two equal-time observations with different literal values.
    let samples = [TimeSeriesSample(id: "boot:run:1", time: 1000, segment: "run"),
        TimeSeriesSample(id: "boot:run:2", time: 1000, segment: "run")]
    let line = TimeSeriesLine(name: "App", color: FlightdeckTheme().cpuSeries, values: [20, 90])
    let model = TimeSeriesModel(samples: samples, lines: [line], domain: 1000...1900)!
    let history = LiveAppHistoryView(key: LiveAppKeyIdentity(kind: .executable, bytes: Data([65])),
        cpu: model, resident: nil, footprint: nil, power: nil,
        selectedTime: 1000, selectedID: "boot:run:2", labels: [], coverage: "Two observations")

    let value = history.selectedValue(model: model, line: line)

    precondition(value == 90, "Repeated-time selection read \(String(describing: value))")
}


@MainActor
private func renders_exact_process_identities_and_readings_in_fixed_native_rows() throws {
    // Goal: native cells retain exact identities and displayed readings without hosting row heights.
    // Method: render the independent process fixture through the real native table coordinator.
    let detail = try liveAppsNativeTableDetail()
    let fixture = LiveAppsNativeTableFixture(data: .processes(detail.processes))

    precondition(fixture.table.tableColumns.map(\.title) == ["PID", "Creation ID", "PID version",
        "Executable", "CPU", "Resident", "Footprint", "Power", "Energy"])
    precondition(!fixture.table.usesAutomaticRowHeights)
    precondition(fixture.table.rowHeight == 26)
    precondition(fixture.scroll.hasVerticalScroller)
    precondition(fixture.scroll.hasHorizontalScroller)
    precondition(fixture.table.accessibilityLabel() == "Exact process creation identities and readings")
    precondition(fixture.field(row: 0, column: 0).stringValue == "42")
    precondition(fixture.field(row: 0, column: 1).stringValue == "9007199254740993")
    precondition(fixture.field(row: 0, column: 1).accessibilityValue()
        == "9007199254740993")
    precondition(liveAppsExposedValues(fixture.field(row: 0, column: 1))
        == ["9007199254740993"])
    precondition(fixture.field(row: 0, column: 2).stringValue == "1")
    precondition(fixture.field(row: 0, column: 3).stringValue == "helper\\xff")
    precondition(fixture.field(row: 0, column: 3).toolTip == "Captured executable")
    precondition(fixture.field(row: 0, column: 4).stringValue == "20 %")
    precondition(fixture.field(row: 0, column: 5).stringValue == "100 MiB")
    precondition(fixture.field(row: 0, column: 6).stringValue == "105 MiB")
    precondition(fixture.field(row: 0, column: 7).stringValue == "1 W")
    precondition(fixture.field(row: 0, column: 8).stringValue == "1 J")
    precondition(fixture.field(row: 2, column: 4).stringValue == "Unavailable refused")
    precondition(fixture.field(row: 2, column: 5).accessibilityValue()
        == "Unavailable unknown")
}

@MainActor
private func renders_group_subtotal_coverage_in_native_cells() throws {
    // Goal: the native group table retains partial coverage beside each subtotal and in help.
    // Method: render two measured members and one refusal from the independent fixture.
    let groups = LiveAppGroups(detail: try liveAppsNativeTableDetail())
    let fixture = LiveAppsNativeTableFixture(data: .groups(groups.rows, selected: nil))

    precondition(fixture.table.tableColumns.map(\.title) == ["Process group", "Instances", "CPU",
        "Resident", "Footprint", "Power", "Energy"])
    precondition(fixture.table.accessibilityLabel() == "Captured executable groups")
    precondition(fixture.field(row: 0, column: 1).stringValue == "3")
    precondition(fixture.field(row: 0, column: 2).stringValue == "50 % 2/3 measured")
    precondition(fixture.field(row: 0, column: 2).toolTip == "2 measured · 1 unavailable")
    precondition(fixture.field(row: 0, column: 3).stringValue == "150 MiB 2/3 measured")
    precondition(fixture.field(row: 0, column: 3).accessibilityValue()
        == "150 MiB 2/3 measured")
    precondition(fixture.field(row: 0, column: 5).stringValue == "3 W 2/3 measured")
}

@MainActor
private func distinguishes_native_measured_zero_from_absent_identity_and_readings() throws {
    // Goal: zero stays measurable while missing identity and refused readings remain explicit.
    // Method: remove identity and replace only known memory/CPU with literal zeros in the fixture.
    let text = String(decoding: LiveAppsFixture.detail, as: UTF8.self)
        .replacingOccurrences(of: #""creation":{"unique_id":"9007199254740993","pid_version":1}"#,
            with: #""creation":null"#)
        .replacingOccurrences(of: #""pid_version":1"#, with: #""pid_version":-70"#)
        .replacingOccurrences(of: #""known":"200000000""#, with: #""known":"0""#)
        .replacingOccurrences(of: #""cpu_percent_one_core":20"#, with: #""cpu_percent_one_core":0"#)
        .replacingOccurrences(of: #""known":"104857600""#, with: #""known":"0""#)
    let detail = try liveAppsNativeTableDetail(Data(text.utf8))
    let fixture = LiveAppsNativeTableFixture(data: .processes(detail.processes))

    precondition(fixture.field(row: 0, column: 1).stringValue == "Unavailable")
    precondition(fixture.field(row: 0, column: 2).stringValue == "Unavailable")
    precondition(fixture.field(row: 1, column: 2).stringValue == "-70")
    precondition(fixture.field(row: 0, column: 4).stringValue == "0 %")
    precondition(fixture.field(row: 0, column: 5).stringValue == "0 GiB")
    precondition(fixture.field(row: 2, column: 5).stringValue == "Unavailable unknown")
}

@MainActor
private func activates_only_known_group_buttons_without_update_callbacks() throws {
    // Goal: only a known group-name button activates its byte key, including after updates.
    // Method: render a known and unknown executable, update, then perform native button clicks.
    let detail = try liveAppsNativeTableDetail()
    let groups = LiveAppGroups(detail: detail).rows
    var initial: [Data?] = []
    var selected: [Data?] = []
    let fixture = LiveAppsNativeTableFixture(data: .groups(groups, selected: nil),
        onSelect: { initial.append($0) })

    fixture.coordinator.apply(fixture.table, data: .groups(groups, selected: nil),
        theme: FlightdeckTheme(), onSelect: { selected.append($0) })
    precondition(initial.isEmpty)
    precondition(selected.isEmpty)
    let knownButton = fixture.button(row: 0)
    knownButton.performClick(nil)
    precondition(initial.isEmpty)
    precondition(selected == [Data([104, 101, 108, 112, 101, 114, 255])])
    fixture.coordinator.apply(fixture.table, data: .groups(groups, selected: groups[0].key),
        theme: FlightdeckTheme(), onSelect: { selected.append($0) })
    precondition(fixture.button(row: 0).attributedTitle.attribute(.foregroundColor,
        at: 0, effectiveRange: nil) as? NSColor == NSColor(FlightdeckTheme().focus))
    precondition(selected.count == 1)

    let text = String(decoding: LiveAppsFixture.detail, as: UTF8.self)
        .replacingOccurrences(of: #"{"bytes_hex":"68656c706572ff","display":"helper\\xff"}"#,
            with: "null")
    let unknown = LiveAppGroups(detail: try liveAppsNativeTableDetail(Data(text.utf8))).rows
    fixture.coordinator.apply(fixture.table, data: .groups(unknown, selected: nil),
        theme: FlightdeckTheme(), onSelect: { selected.append($0) })

    precondition(selected.count == 1)
    knownButton.performClick(nil)
    precondition(selected.count == 1)
    precondition(!fixture.button(row: 0).isEnabled)
    fixture.button(row: 0).performClick(nil)
    precondition(selected.count == 1)
}

@MainActor
private func refreshes_reused_native_cells_without_stale_text_or_help() {
    // Goal: recycling a native field clears old coverage, help and accessibility values.
    // Method: update the same field from a partial subtotal to a measured zero.
    let field = LiveAppsTableTextField(labelWithString: "")
    let partial = LiveAppsTableCell(value: "150 MiB", annotation: "2/3 measured",
        help: "2 measured · 1 unavailable")
    field.apply(partial, theme: FlightdeckTheme())

    field.apply(LiveAppsTableCell(value: "0 GiB"), theme: FlightdeckTheme())

    precondition(field.stringValue == "0 GiB")
    precondition(field.accessibilityValue() == "0 GiB")
    precondition(field.toolTip == nil)
    precondition(field.maximumNumberOfLines == 1)
    precondition(liveAppsExposedValues(field) == ["0 GiB"])
}

@MainActor
private func applies_native_live_table_theme_without_enabling_row_selection() throws {
    // Goal: native table refreshes adopt theme changes while group buttons retain sole activation.
    // Method: apply a larger contrasting theme to the real table and inspect its refreshed cells.
    let detail = try liveAppsNativeTableDetail()
    let data = LiveAppsTableData.processes(detail.processes)
    let fixture = LiveAppsNativeTableFixture(data: data)
    let original = fixture.displayedField(row: 0, column: 1)
    precondition(original.font?.pointSize == FlightdeckTheme().tableFontSize)
    var theme = FlightdeckTheme()
    theme.tableFontSize = 19
    theme.text = .yellow
    theme.panel = .black

    fixture.coordinator.apply(fixture.table, data: data, theme: theme, onSelect: nil)

    fixture.table.layoutSubtreeIfNeeded()
    precondition(fixture.displayedField(row: 0, column: 1).font?.pointSize == 19)
    precondition(fixture.displayedField(row: 0, column: 1).textColor == NSColor(theme.text))
    precondition(fixture.table.tableColumns[0].headerCell.font?.pointSize == 19)
    precondition(fixture.table.backgroundColor == NSColor(theme.panel))
    precondition(!fixture.coordinator.tableView(fixture.table, shouldSelectRow: 0))
}

private func liveAppsNativeTableDetail(_ data: Data = LiveAppsFixture.detail) throws -> LiveAppDetailPage {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try decoder.decode(LiveAppDetailPage.self, from: data)
}

@MainActor
private func liveAppsExposedValues(_ field: NSTextField) -> [String] {
    NSAccessibility.unignoredChildrenForOnlyChild(from: field).compactMap {
        ($0 as? any NSAccessibilityProtocol)?.accessibilityValue() as? String
    }
}

@MainActor
private struct LiveAppsNativeTableFixture {
    let coordinator: LiveAppsNativeTable.Coordinator
    let scroll: NSScrollView
    let table: NSTableView

    init(data: LiveAppsTableData, onSelect: ((Data?) -> Void)? = nil) {
        coordinator = LiveAppsNativeTable.Coordinator()
        scroll = coordinator.makeScrollView()
        guard let table = scroll.documentView as? NSTableView else { preconditionFailure() }
        self.table = table
        coordinator.apply(table, data: data, theme: FlightdeckTheme(), onSelect: onSelect)
        scroll.frame = NSRect(x: 0, y: 0, width: 1400, height: 240)
        scroll.tile()
        table.layoutSubtreeIfNeeded()
    }

    func displayedField(row: Int, column: Int) -> NSTextField {
        guard let field = table.view(atColumn: column, row: row, makeIfNecessary: true)
            as? NSTextField else { preconditionFailure() }

        return field
    }

    func field(row: Int, column: Int) -> NSTextField {
        guard let field = coordinator.tableView(table, viewFor: table.tableColumns[column], row: row)
            as? NSTextField else { preconditionFailure() }
        return field
    }

    func button(row: Int) -> NSButton {
        guard let button = coordinator.tableView(table, viewFor: table.tableColumns[0], row: row)
            as? NSButton else { preconditionFailure() }
        return button
    }
}


@MainActor
private func renders_overflowed_group_subtotals_as_unavailable() {
    // Goal: an overflowed subtotal never looks like a known reading or a complete measurement.
    // Method: render a group with a literal overflowed measure through its native cell.
    let measure = LiveAppMeasure(known: LiveAppCounter(9), measuredMembers: 1,
        unavailableMembers: 1, overflowed: true)
    let group = LiveAppProcessGroup(key: nil, displayName: "Unknown executable", instances: 2,
        cpuNs: measure, energyNj: measure, residentBytes: measure, footprintBytes: measure,
        cpuPercentOneCore: nil, energyJoules: nil, energyWatts: nil)
    let fixture = LiveAppsNativeTableFixture(data: .groups([group], selected: nil))

    precondition(fixture.field(row: 0, column: 3).stringValue
        == "Unavailable 1/2 measured · overflow")
    precondition(fixture.field(row: 0, column: 3).toolTip
        == "1 measured · 1 unavailable · overflow")
}

@MainActor
private func renders_the_loaded_process_bound_and_clears_an_empty_page() throws {
    // Goal: the native adapter handles the full loaded page and releases its rows on an empty page.
    // Method: render 512 copies of a decoded process, then replace them with no processes.
    let process = try liveAppsNativeTableDetail().processes[0]
    let fixture = LiveAppsNativeTableFixture(data: .processes(Array(repeating: process, count: 512)))

    precondition(fixture.table.numberOfRows == 512)
    precondition(fixture.field(row: 511, column: 1).stringValue == "9007199254740993")
    fixture.coordinator.apply(fixture.table, data: .processes([]),
        theme: FlightdeckTheme(), onSelect: nil)

    precondition(fixture.table.numberOfRows == 0)
    precondition(fixture.table.tableColumns.count == 9)
}
