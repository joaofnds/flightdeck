import SwiftUI

struct RecordedStateTimeline<State> {
    static var observationsMax: Int { 4096 }
    let firstSecond: UInt64
    let endSecond: UInt64
    let observations: [Metric]
    let cells: [Int?]
    let selectedID: String?
    private let stateOf: (Metric) -> State?

    init?(selection: Selection, stateOf: @escaping (Metric) -> State?) {
        guard selection.contextSeconds == 900,
              selection.trends.count <= Self.observationsMax,
              selection.second <= UInt64.max / 1_000_000_000 else { return nil }

        self.stateOf = stateOf
        firstSecond = selection.second >= 899 ? selection.second - 899 : 0
        endSecond = selection.second + 1
        observations = Self.visibleObservations(selection, firstSecond: firstSecond,
            endSecond: endSecond)
        selectedID = RecordedChartContext(selection: selection).selectedID
        var cells = [Int?](repeating: nil, count: Int(endSecond - firstSecond))
        var identities = Set<String>()

        for (index, point) in observations.enumerated() {
            guard identities.insert(point.id).inserted else { return nil }

            let cell = Int(point.wallNs / 1_000_000_000 - firstSecond)
            if let previous = cells[cell], observations[previous].id == selectedID { continue }

            cells[cell] = index
        }

        self.cells = cells
    }

    private static func visibleObservations(_ selection: Selection, firstSecond: UInt64,
                                             endSecond: UInt64) -> [Metric] {
        selection.trends.filter { point in
            let second = point.wallNs / 1_000_000_000

            return second >= firstSecond && second < endSecond
        }.sorted {
            ($0.wallNs, $0.run, $0.boot, $0.publication)
                < ($1.wallNs, $1.run, $1.boot, $1.publication)
        }
    }

    func point(at fraction: Double) -> Metric? {
        guard fraction.isFinite, (0..<1).contains(fraction) else { return nil }

        let cell = Int(fraction * Double(cells.count))
        guard let index = cells[cell] else { return nil }

        return observations[index]
    }

    func point(id: String?) -> Metric? {
        guard let id else { return nil }

        return observations.first { $0.id == id }
    }

    func state(id: String?) -> State? {
        point(id: id).flatMap(stateOf)
    }

    func state(at index: Int) -> State? {
        guard observations.indices.contains(index) else { return nil }

        return stateOf(observations[index])
    }

    func target(id: String?) -> RecordedSelectionTarget? {
        guard let point = point(id: id) else { return nil }

        return RecordedSelectionTarget(second: point.wallNs / 1_000_000_000,
            identity: point.identity)
    }

    func step(id: String?, offset: Int) -> Metric? {
        precondition((-1...1).contains(offset))
        guard !observations.isEmpty else { return nil }

        let current = observations.firstIndex { $0.id == (id ?? selectedID) }
            ?? observations.count - 1
        let next = min(observations.count - 1, max(0, current + offset))

        return observations[next]
    }

    func boundary(first: Bool) -> Metric? {
        first ? observations.first : observations.last
    }

    func fraction(id: String?) -> Double? {
        guard let point = point(id: id) else { return nil }

        let elapsed = point.wallNs / 1_000_000_000 - firstSecond
        let fraction = Double(point.wallNs % 1_000_000_000) / 1e9

        return (Double(elapsed) + fraction) / Double(cells.count)
    }
}

@MainActor
struct RecordedStateInteraction<State> {
    let timeline: RecordedStateTimeline<State>
    @Binding var inspectedID: String?
    @Binding var inspecting: Bool
    let busy: Bool
    let onSelect: (RecordedSelectionTarget) -> Void

    var readoutID: String? { inspecting ? inspectedID : timeline.selectedID }

    func inspect(at point: CGPoint, size: CGSize) {
        guard !busy else { return }

        let frame = RecordedStatePlotLayout.frame(size: size)

        inspecting = true
        inspectedID = nil

        if frame.contains(point), frame.width > 0 {
            inspectedID = timeline.point(at: (point.x - frame.minX) / frame.width)?.id
        }
    }

    @discardableResult
    func click(at point: CGPoint, size: CGSize) -> KeyPress.Result {
        guard !busy else { return .ignored }

        inspect(at: point, size: size)

        return select()
    }

    func step(offset: Int) -> KeyPress.Result {
        guard !busy, let point = timeline.step(id: readoutID, offset: offset) else {
            return .ignored
        }

        inspectedID = point.id
        inspecting = true

        return .handled
    }

    func boundary(first: Bool) -> KeyPress.Result {
        guard !busy, let point = timeline.boundary(first: first) else { return .ignored }

        inspectedID = point.id
        inspecting = true

        return .handled
    }

    @discardableResult
    func clear() -> KeyPress.Result {
        guard !busy else { return .ignored }

        resetInspection()

        return .handled
    }

    func select() -> KeyPress.Result {
        guard !busy, let target = timeline.target(id: readoutID) else { return .ignored }

        resetInspection()
        onSelect(target)

        return .handled
    }

    private func resetInspection() {
        inspectedID = nil
        inspecting = false
    }
}

enum RecordedStatePlotLayout {
    static func frame(size: CGSize) -> CGRect {
        CGRect(x: 50, y: max(8, (size.height - 52) / 2),
            width: max(0, size.width - 115), height: 28)
    }
}
