import AppKit
import SwiftUI

struct RecordedTable: NSViewRepresentable {
    @Environment(\.flightdeckTheme) private var theme
    let page: TablePage
    let fields: [String]
    @Binding var selection: Int?

    func makeCoordinator() -> Coordinator { Coordinator(selection: $selection) }

    func makeNSView(context: Context) -> NSScrollView {
        let table = NSTableView()
        table.delegate = context.coordinator
        table.dataSource = context.coordinator
        table.allowsMultipleSelection = false
        table.allowsEmptySelection = true
        table.columnAutoresizingStyle = .noColumnAutoresizing
        table.rowHeight = 26
        table.intercellSpacing = NSSize(width: 10, height: 2)
        table.setAccessibilityLabel("Recorded \(page.name)")
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? NSTableView else { preconditionFailure() }
        let coordinator = context.coordinator
        coordinator.selection = $selection
        coordinator.apply(table, page: page, fields: fields, theme: theme)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView,
                     context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 600, height: 330)
    }

    @MainActor
    final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        var page: TablePage?
        var theme = FlightdeckTheme()
        var selection: Binding<Int?>
        var updating = false

        init(selection: Binding<Int?>) { self.selection = selection }

        func apply(_ table: NSTableView, page: TablePage, fields: [String],
                   theme: FlightdeckTheme) {
            updating = true
            defer { updating = false }
            self.page = page
            self.theme = theme
            table.backgroundColor = NSColor(theme.panel)
            if table.tableColumns.map(\.identifier.rawValue) != fields {
                for column in table.tableColumns { table.removeTableColumn(column) }
                for field in fields {
                    let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(field))
                    column.title = field.replacingOccurrences(of: "_", with: " ")
                    column.width = 180
                    column.minWidth = 130
                    table.addTableColumn(column)
                }
            }
            for column in table.tableColumns {
                column.headerCell.font = theme.tableTypeface
            }
            table.reloadData()
            if let selection = selection.wrappedValue, selection < page.rows.count {
                table.selectRowIndexes(IndexSet(integer: selection), byExtendingSelection: false)
            } else {
                table.deselectAll(nil)
            }
        }

        func numberOfRows(in tableView: NSTableView) -> Int { page?.rows.count ?? 0 }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?,
                       row: Int) -> NSView? {
            guard let page, let tableColumn, page.rows.indices.contains(row) else { return nil }
            let identifier = NSUserInterfaceItemIdentifier("recorded-value")
            let field = tableView.makeView(withIdentifier: identifier, owner: nil) as? NSTextField
                ?? NSTextField(labelWithString: "")
            field.identifier = identifier
            field.font = theme.tableTypeface
            field.textColor = NSColor(theme.text)
            field.stringValue = page.value(page.rows[row], tableColumn.identifier.rawValue)
                ?? "Unavailable"
            field.lineBreakMode = .byTruncatingMiddle
            field.isSelectable = true
            field.setAccessibilityValue(field.stringValue)
            return field
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table = notification.object as? NSTableView else { return }
            selection.wrappedValue = table.selectedRow >= 0 ? table.selectedRow : nil
        }
    }
}
