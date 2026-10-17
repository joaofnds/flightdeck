import Foundation

struct DashboardQuantity: Equatable {
    let value: Double
    let unit: String

    init?(value: Double?, unit: String) {
        guard let value, value.isFinite else { return nil }
        let magnitude = abs(value)
        switch unit {
        case "W" where magnitude > 0 && magnitude < 1:
            self.value = value * 1000
            self.unit = "mW"
        case "MB/s" where magnitude > 0 && magnitude < 0.001:
            self.value = value * 1_000_000
            self.unit = "B/s"
        case "MB/s" where magnitude > 0 && magnitude < 1:
            self.value = value * 1000
            self.unit = "kB/s"
        case "GiB" where magnitude > 0 && magnitude < 1:
            self.value = value * 1024
            self.unit = "MiB"
        case "% of one core", "% of interval":
            self.value = value
            self.unit = "%"
        default:
            self.value = value
            self.unit = unit
        }
    }

    func text(digits: Int? = nil) -> String {
        let precision = digits.map { max(0, min(12, $0)) }
        let number: String
        if let precision, abs(value) >= pow(10, -Double(precision)) || value == 0 {
            number = value.formatted(.number.precision(.fractionLength(precision)))
        } else {
            number = value.formatted(.number.precision(.significantDigits(1...3)))
        }
        return number + (unit.isEmpty ? "" : " " + unit)
    }
}

func dashboardReading(_ value: Double?, unit: String, digits: Int? = nil) -> String {
    DashboardQuantity(value: value, unit: unit)?.text(digits: digits) ?? "Unavailable"
}
