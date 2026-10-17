import AppKit
import SwiftUI

struct LiveAppsNativeTable: NSViewRepresentable {
    @Environment(\.flightdeckTheme) private var theme
    let data: LiveAppsTableData
    let onSelect: ((Data?) -> Void)?

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSScrollView {
        context.coordinator.makeScrollView()
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? NSTableView else { preconditionFailure() }

        context.coordinator.apply(table, data: data, theme: theme, onSelect: onSelect)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView,
                     context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 600, height: proposal.height ?? theme.chartHeight)
    }

    @MainActor
    final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        private var data: LiveAppsTableData?
        private var theme = FlightdeckTheme()
        private var onSelect: ((Data?) -> Void)?

        func makeScrollView() -> NSScrollView {
            let table = NSTableView()
            table.delegate = self
            table.dataSource = self
            table.usesAutomaticRowHeights = false
            table.rowHeight = 26
            table.intercellSpacing = NSSize(width: 10, height: 2)
            table.allowsMultipleSelection = false
            table.allowsEmptySelection = true
            table.selectionHighlightStyle = .none
            table.columnAutoresizingStyle = .noColumnAutoresizing

            let scroll = NSScrollView()
            scroll.documentView = table
            scroll.hasVerticalScroller = true
            scroll.hasHorizontalScroller = true
            scroll.autohidesScrollers = true

            return scroll
        }

        func apply(_ table: NSTableView, data: LiveAppsTableData, theme: FlightdeckTheme,
                   onSelect: ((Data?) -> Void)?) {
            precondition(data.rows.count <= 512)
            precondition(data.columns.count <= 9)
            precondition(data.rows.allSatisfy { $0.cells.count == data.columns.count })
            let appearanceChanged = self.theme.tableFontSize != theme.tableFontSize
                || self.theme.text != theme.text || self.theme.muted != theme.muted
                || self.theme.focus != theme.focus || self.theme.panel != theme.panel
            let contentsChanged = self.data != data
            let columnsChanged = self.data?.columns != data.columns

            self.data = data
            self.theme = theme
            self.onSelect = onSelect
            table.backgroundColor = NSColor(theme.panel)
            table.setAccessibilityLabel(data.label)
            if columnsChanged { applyColumns(table, columns: data.columns) }
            for column in table.tableColumns { column.headerCell.font = theme.tableTypeface }
            if contentsChanged || appearanceChanged { table.reloadData() }
        }

        private func applyColumns(_ table: NSTableView, columns: [LiveAppsTableColumn]) {
            for column in table.tableColumns { table.removeTableColumn(column) }

            for (index, specification) in columns.enumerated() {
                let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(String(index)))
                column.title = specification.title
                column.minWidth = specification.minimum
                column.width = specification.ideal
                table.addTableColumn(column)
            }
        }

        func numberOfRows(in tableView: NSTableView) -> Int { data?.rows.count ?? 0 }

        func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { false }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?,
                       row: Int) -> NSView? {
            guard let data, let tableColumn, data.rows.indices.contains(row),
                  let column = Int(tableColumn.identifier.rawValue),
                  data.columns.indices.contains(column) else { return nil }

            if data.columns[column].activatesGroup {
                return groupButton(tableView, row: data.rows[row])
            }

            let identifier = NSUserInterfaceItemIdentifier("live-app-value")
            let field = tableView.makeView(withIdentifier: identifier, owner: nil)
                as? LiveAppsTableTextField ?? LiveAppsTableTextField(labelWithString: "")

            field.identifier = identifier
            field.apply(data.rows[row].cells[column], theme: theme)

            return field
        }

        private func groupButton(_ table: NSTableView, row: LiveAppsTableRow) -> NSButton {
            let identifier = NSUserInterfaceItemIdentifier("live-app-group")
            let button = table.makeView(withIdentifier: identifier, owner: nil)
                as? LiveAppsGroupButton ?? LiveAppsGroupButton(title: "", target: nil, action: nil)

            button.identifier = identifier
            button.groupKey = row.groupKey
            button.title = row.cells[0].value
            button.font = theme.tableTypeface
            button.contentTintColor = NSColor(row.selected ? theme.focus : theme.text)
            button.attributedTitle = NSAttributedString(string: button.title,
                attributes: [.font: theme.tableTypeface,
                    .foregroundColor: NSColor(row.selected ? theme.focus : theme.text)])
            button.isBordered = false
            button.alignment = .left
            button.lineBreakMode = .byTruncatingTail
            button.isEnabled = row.groupKey != nil
            button.target = self
            button.action = #selector(activateGroup(_:))
            button.setAccessibilityLabel(button.title)

            return button
        }

        @objc private func activateGroup(_ button: LiveAppsGroupButton) {
            guard button.isEnabled, let key = button.groupKey,
                  data?.rows.contains(where: { $0.groupKey == key }) == true else { return }

            onSelect?(key)
        }
    }
}

final class LiveAppsGroupButton: NSButton {
    var groupKey: Data?
}

final class LiveAppsTableTextField: NSTextField {
    func apply(_ cell: LiveAppsTableCell, theme: FlightdeckTheme) {
        font = theme.tableTypeface
        textColor = NSColor(theme.text)
        lineBreakMode = .byTruncatingTail
        maximumNumberOfLines = 1
        isSelectable = false

        let value = NSMutableAttributedString(string: cell.value,
            attributes: [.font: theme.tableTypeface, .foregroundColor: NSColor(theme.text)])
        if !cell.annotation.isEmpty {
            value.append(NSAttributedString(string: " " + cell.annotation,
                attributes: [.font: NSFont.preferredFont(forTextStyle: .caption2, options: [:]),
                    .foregroundColor: NSColor(theme.muted)]))
        }

        attributedStringValue = value
        toolTip = cell.help
        setAccessibilityValue(value.string)
    }
}

struct LiveAppsTableData: Equatable {
    let label: String
    let columns: [LiveAppsTableColumn]
    let rows: [LiveAppsTableRow]

    static func groups(_ groups: [LiveAppProcessGroup], selected: Data?) -> Self {
        precondition(groups.count <= 512)
        let columns = [LiveAppsTableColumn(title: "Process group", minimum: 160, ideal: 230,
                activatesGroup: true),
            LiveAppsTableColumn(title: "Instances", minimum: 65, ideal: 75)]
            + ["CPU", "Resident", "Footprint", "Power", "Energy"].map {
                LiveAppsTableColumn(title: $0, minimum: 130, ideal: 180)
            }
        let rows = groups.map { group in
            LiveAppsTableRow(cells: [LiveAppsTableCell(value: group.displayName),
                LiveAppsTableCell(value: String(group.instances)),
                .measure(dashboardReading(group.cpuPercentOneCore, unit: "% of one core"), group.cpuNs),
                .measure(liveAppMemory(group.residentBytes.knownValue), group.residentBytes),
                .measure(liveAppMemory(group.footprintBytes.knownValue), group.footprintBytes),
                .measure(dashboardReading(group.energyWatts, unit: "W"), group.energyNj),
                .measure(dashboardReading(group.energyJoules, unit: "J"), group.energyNj)],
                groupKey: group.key, selected: selected == group.key)
        }

        return Self(label: "Captured executable groups", columns: columns, rows: rows)
    }

    static func processes(_ processes: [LiveAppProcess]) -> Self {
        precondition(processes.count <= 512)
        let columns = [LiveAppsTableColumn(title: "PID", minimum: 60, ideal: 70),
            LiveAppsTableColumn(title: "Creation ID", minimum: 165, ideal: 180),
            LiveAppsTableColumn(title: "PID version", minimum: 75, ideal: 90),
            LiveAppsTableColumn(title: "Executable", minimum: 150, ideal: 200)]
            + ["CPU", "Resident", "Footprint", "Power", "Energy"].map {
                LiveAppsTableColumn(title: $0, minimum: 140, ideal: 180)
            }
        let rows = processes.map { process in
            LiveAppsTableRow(cells: [LiveAppsTableCell(value: String(process.pid)),
                LiveAppsTableCell(value: process.creation.map { String($0.uniqueId.value) }
                    ?? "Unavailable"),
                LiveAppsTableCell(value: process.creation.map { String($0.pidVersion) }
                    ?? "Unavailable"),
                LiveAppsTableCell(value: process.executableName?.display ?? "Unavailable",
                    help: process.appAttribution.map { "App attribution: " + $0.rawValue }
                        ?? "Captured executable"),
                .amount(dashboardReading(process.cpuPercentOneCore, unit: "% of one core"), process.cpuNs),
                .amount(liveAppMemory(process.residentBytes.known?.value), process.residentBytes),
                .amount(liveAppMemory(process.footprintBytes.known?.value), process.footprintBytes),
                .amount(dashboardReading(process.energyWatts, unit: "W"), process.energyNj),
                .amount(dashboardReading(process.energyJoules, unit: "J"), process.energyNj)],
                groupKey: nil, selected: false)
        }

        return Self(label: "Exact process creation identities and readings", columns: columns, rows: rows)
    }
}

struct LiveAppsTableColumn: Equatable {
    let title: String
    let minimum: CGFloat
    let ideal: CGFloat
    var activatesGroup = false
}

struct LiveAppsTableRow: Equatable {
    let cells: [LiveAppsTableCell]
    let groupKey: Data?
    let selected: Bool
}

struct LiveAppsTableCell: Equatable {
    let value: String
    var annotation = ""
    var help: String?

    static func measure(_ value: String, _ measure: LiveAppMeasure) -> Self {
        var annotation = ""
        if measure.unavailableMembers > 0 || measure.overflowed {
            annotation = "\(measure.measuredMembers)/\(UInt64(measure.measuredMembers) + UInt64(measure.unavailableMembers)) measured"
                + (measure.overflowed ? " · overflow" : "")
        }

        return Self(value: value, annotation: annotation, help: measure.coverage)
    }

    static func amount(_ value: String, _ amount: LiveAppAmount) -> Self {
        Self(value: value,
            annotation: amount.unavailable?.rawValue.replacingOccurrences(of: "_", with: " ") ?? "")
    }
}
