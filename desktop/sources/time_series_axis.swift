import Foundation

struct TimeSeriesAxis {
    let domain: ClosedRange<Double>
    let ticks: [Double]
    let unit: String
    private let multiplier: Double

    init(range: ClosedRange<Double>, unit: String, minimum: Double? = nil,
         maximum: Double? = nil) {
        precondition(range.lowerBound.isFinite)
        precondition(range.upperBound.isFinite)

        var lower = minimum ?? min(0, range.lowerBound)
        var upper = maximum ?? max(0, range.upperBound)
        if upper <= lower { upper = max(lower + 1, lower.nextUp) }
        let step = Self.step(span: upper - lower, unit: unit)
        if minimum == nil { lower = floor(lower / step) * step }
        if maximum == nil { upper = ceil(upper / step) * step }
        precondition(upper > lower)
        precondition(upper.isFinite)
        precondition(lower.isFinite)

        domain = lower...upper
        let first = ceil(lower / step)
        var ticks: [Double] = []
        for offset in 0..<8 {
            let value = (first + Double(offset)) * step
            if value >= lower - step * 1e-9, value <= upper + step * 1e-9 {
                let bounded = max(lower, min(upper, value))
                if ticks.last != bounded { ticks.append(bounded) }
            }
        }
        self.ticks = ticks

        let magnitude = max(abs(lower), abs(upper))
        if let quantity = DashboardQuantity(value: magnitude, unit: unit), magnitude > 0 {
            multiplier = quantity.value / magnitude
            self.unit = quantity.unit
        } else {
            multiplier = 1
            self.unit = unit
        }
    }

    func label(_ value: Double) -> String {
        let displayed = value * multiplier
        let magnitude = abs(displayed)
        let notation: FloatingPointFormatStyle<Double>.Configuration.Notation
        if magnitude >= 10_000 { notation = .compactName }
        else {
            if magnitude > 0, magnitude < 0.001 { notation = .scientific }
            else { notation = .automatic }
        }

        return displayed.formatted(.number.notation(notation).precision(.significantDigits(1...3)))
    }

    private static func step(span: Double, unit: String) -> Double {
        let target = max(Double.leastNormalMagnitude, span / 5)
        if unit == "GiB" { return pow(2, ceil(log2(target))) }

        let magnitude = pow(10, floor(log10(target)))
        let fraction = target / magnitude
        for factor in [1.0, 2.0, 5.0, 10.0] {
            if fraction <= factor { return factor * magnitude }
        }
        preconditionFailure("A decimal interval must fit within its next power of ten")
    }
}
