//! One series' readings reduced over a stretch of the clock, so a reading that lasted a second
//! stays findable however far a chart thins the series. Each reduction keeps what a reader needs
//! to find that reading again, and the summaries of adjacent stretches merge into exactly the
//! summary of their readings, however the readings were cut.

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;

/// The fractional bits of a total. A mean's total is an integer so that adding totals gives one
/// answer in any grouping, which adding floats does not, and a thousandth of a unit is finer than
/// any chart draws.
const TOTAL_FRACTION_BITS: u64 = 10;
/// Past this many whole bits a reading is no quantity the Mac reports, and it counts as missing.
const READING_WHOLE_BITS_MAX: u64 = 53;
const _: () = {
    assert!(READING_WHOLE_BITS_MAX + TOTAL_FRACTION_BITS < 64);
    // A summary holds no buffer, so what a chart keeps follows its series and its tier and not the
    // recording's length.
    assert!(size_of::<GaugeSummary<u32>>() <= 128);
    assert!(size_of::<RateSummary<u32>>() <= 128);
};

/// One value of a reduction and the reading it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extreme<Place> {
    pub value: f64,
    pub place: Place,
}

/// What a reduction found of readings with values: the lowest and the highest, the earliest of
/// any that tie, so a merge finds the one a reduction of all of them finds, and the newest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured<Place> {
    pub minimum: Extreme<Place>,
    pub maximum: Extreme<Place>,
    pub last: Extreme<Place>,
    /// How many readings had values.
    pub count: u32,
    total: i128,
    weight: i128,
    overflowed: bool,
}

/// What a gauge and a rate share: the readings with values, and how many had none or did not
/// follow the reading before them. A reading of zero is a value. A reading that did not follow
/// marks a break in the line a reader must not draw across.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reduction<Place> {
    pub measured: Option<Measured<Place>>,
    pub missing: u32,
    pub breaks: u32,
}

impl<Place: Copy> Reduction<Place> {
    /// The mean of the readings over what they were weighted by, or `None` with no reading or
    /// where the total outgrew what it counts. The division truncates a fraction of the total's
    /// finest unit.
    #[must_use]
    pub fn mean(&self) -> Option<f64> {
        let measured = self.measured.as_ref().filter(|found| !found.overflowed)?;

        Some(value_from_fixed(measured.total / measured.weight))
    }

    fn push(&mut self, value: Option<f64>, weight: u64, follows: bool, place: Place) {
        if !follows {
            self.breaks += 1;
        }
        let fixed = value.and_then(fixed_from_value);
        let (Some(value), Some(fixed)) = (value, fixed) else {
            self.missing += 1;
            return;
        };

        let mark = Extreme { value, place };
        let total = fixed.checked_mul(i128::from(weight));
        self.merge_measured(Measured {
            minimum: mark,
            maximum: mark,
            last: mark,
            count: 1,
            total: total.unwrap_or(0),
            weight: i128::from(weight),
            overflowed: total.is_none(),
        });
    }

    /// Takes in the reduction of the readings right after this one's.
    fn merge(&mut self, next: &Self) {
        self.missing += next.missing;
        self.breaks += next.breaks;
        if let Some(measured) = next.measured {
            self.merge_measured(measured);
        }
    }

    fn merge_measured(&mut self, next: Measured<Place>) {
        let Some(held) = &mut self.measured else {
            self.measured = Some(next);
            return;
        };

        if next.minimum.value < held.minimum.value {
            held.minimum = next.minimum;
        }
        if next.maximum.value > held.maximum.value {
            held.maximum = next.maximum;
        }
        held.last = next.last;
        held.count += next.count;
        let total = held.total.checked_add(next.total);
        held.total = total.unwrap_or(0);
        held.weight += next.weight;
        held.overflowed = held.overflowed || next.overflowed || total.is_none();
    }
}

/// Readings of a quantity at an instant, such as a temperature or the memory in use, each counting
/// the same in a mean.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GaugeSummary<Place>(Reduction<Place>);

impl<Place: Copy> GaugeSummary<Place> {
    /// Takes in the reading after the last one. `None` is a reading the collector did not read.
    pub fn push(&mut self, value: Option<f64>, follows: bool, place: Place) {
        self.0.push(value, 1, follows, place);
    }

    /// Takes in the summary of the readings right after this one's.
    pub fn merge(&mut self, next: &Self) {
        self.0.merge(&next.0);
    }
}

impl<Place> std::ops::Deref for GaugeSummary<Place> {
    type Target = Reduction<Place>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Rates a counter's growth gave over intervals between readings, each counting in a mean for as
/// long as its interval lasted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RateSummary<Place>(Reduction<Place>);

impl<Place: Copy> RateSummary<Place> {
    /// Takes in the rate over the `elapsed_ns` after the last reading, or `None` where the two
    /// readings give no rate.
    pub fn push(&mut self, rate: Option<(f64, u64)>, follows: bool, place: Place) {
        let elapsed_ns = rate.map_or(0, |(_rate, elapsed_ns)| elapsed_ns);
        let value = rate.filter(|_| elapsed_ns > 0).map(|(rate, _)| rate);
        self.0.push(value, elapsed_ns, follows, place);
    }

    /// Takes in the summary of the readings right after this one's.
    pub fn merge(&mut self, next: &Self) {
        self.0.merge(&next.0);
    }

    /// The time the rates covered, in nanoseconds, or `None` with no rate. Time between readings
    /// that gave no rate is no part of it.
    #[must_use]
    pub fn covered_ns(&self) -> Option<i128> {
        self.0.measured.as_ref().map(|measured| measured.weight)
    }
}

impl<Place> std::ops::Deref for RateSummary<Place> {
    type Target = Reduction<Place>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One category and the reading that showed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark<Category, Place> {
    pub category: Category,
    pub place: Place,
}

/// What readings of a category showed, which no mean describes. Keeps the changes between
/// categories rather than a middle one, the first `CHANGES_KEPT` of them in order and a count of
/// them all, so a reader knows when any were left out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CategorySummary<Category, Place, const CHANGES_KEPT: usize> {
    pub first: Option<Mark<Category, Place>>,
    pub last: Option<Mark<Category, Place>>,
    /// The highest category, its earliest reading.
    pub highest: Option<Mark<Category, Place>>,
    kept: [Option<Mark<Category, Place>>; CHANGES_KEPT],
    pub change_count: u32,
    pub missing: u32,
    pub breaks: u32,
}

impl<Category, Place, const CHANGES_KEPT: usize> Default
    for CategorySummary<Category, Place, CHANGES_KEPT>
where
    Category: Copy,
    Place: Copy,
{
    fn default() -> Self {
        Self {
            first: None,
            last: None,
            highest: None,
            kept: [None; CHANGES_KEPT],
            change_count: 0,
            missing: 0,
            breaks: 0,
        }
    }
}

impl<Category, Place, const CHANGES_KEPT: usize> CategorySummary<Category, Place, CHANGES_KEPT>
where
    Category: Copy + Ord,
    Place: Copy,
{
    /// Takes in the reading after the last one. `None` is a reading the collector did not read.
    pub fn push(&mut self, category: Option<Category>, follows: bool, place: Place) {
        if !follows {
            self.breaks += 1;
        }
        let Some(category) = category else {
            self.missing += 1;
            return;
        };

        let mark = Mark { category, place };
        self.merge(&Self {
            first: Some(mark),
            last: Some(mark),
            highest: Some(mark),
            ..Self::default()
        });
    }

    /// Takes in the summary of the readings right after this one's.
    pub fn merge(&mut self, next: &Self) {
        self.missing += next.missing;
        self.breaks += next.breaks;
        let Some(next_first) = next.first else {
            return;
        };

        let Some(last) = self.last else {
            self.first = next.first;
            self.last = next.last;
            self.highest = next.highest;
            self.kept = next.kept;
            self.change_count = next.change_count;
            return;
        };

        if last.category != next_first.category {
            self.merge_change(next_first);
        }
        for change in next.kept.iter().flatten() {
            self.merge_change(*change);
        }
        self.change_count += next.change_count.saturating_sub(next.kept_count());
        self.last = next.last;
        self.highest = match (self.highest, next.highest) {
            (Some(held), Some(found)) if found.category > held.category => Some(found),
            (held, _) => held,
        };
    }

    /// The changes kept, oldest first, each the reading that first showed the new category.
    pub fn changes(&self) -> impl Iterator<Item = &Mark<Category, Place>> {
        self.kept.iter().flatten()
    }

    fn kept_count(&self) -> u32 {
        crate::u32_from_length(self.kept.iter().flatten().count())
    }

    fn merge_change(&mut self, change: Mark<Category, Place>) {
        if let Some(room) = self.kept.iter_mut().find(|kept| kept.is_none()) {
            *room = Some(change);
        }
        self.change_count += 1;
    }
}

/// The reading of the Mac a reduction found its value at, which holding it shows again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MachinePlace {
    pub boot: [u64; 2],
    pub run: [u64; 2],
    pub publication: u64,
    pub wall_ns: u64,
}

impl MachinePlace {
    /// The wall second of the reading. Floors, so a reading belongs to the second it was taken in.
    #[must_use]
    pub fn second(&self) -> u64 {
        self.wall_ns / 1_000_000_000
    }
}

/// `value` in the total's fixed point, rounded toward zero, which keeps its grouping exact. Reads
/// the float's own bits, since a cast is what the project does not write. `None` where `value` is
/// not finite or no quantity the Mac reports.
fn fixed_from_value(value: f64) -> Option<i128> {
    const FRACTION_MASK: u64 = (1 << 52) - 1;
    const EXPONENT_MAX: u64 = 0x7ff;
    const EXPONENT_ZERO: u64 = 1023;

    let bits = value.to_bits();
    let exponent = (bits >> 52) & EXPONENT_MAX;
    if exponent == EXPONENT_MAX {
        return None;
    }
    // Below one unit of the total's fraction, and a subnormal is far below it, a reading is zero.
    if exponent + TOTAL_FRACTION_BITS < EXPONENT_ZERO {
        return Some(0);
    }

    if exponent + 1 > EXPONENT_ZERO + READING_WHOLE_BITS_MAX {
        return None;
    }
    let significand = u128::from((bits & FRACTION_MASK) | (1 << 52));
    let scale = exponent + TOTAL_FRACTION_BITS;
    let aligned = EXPONENT_ZERO + 52;
    let magnitude = if scale >= aligned {
        significand << (scale - aligned)
    } else {
        significand >> (aligned - scale)
    };
    // At most `READING_WHOLE_BITS_MAX` whole bits and the fraction, which the const assertion above
    // keeps under 64.
    let magnitude = i128::try_from(magnitude).unwrap_or_else(|_| unreachable!());

    if bits >> 63 == 1 {
        Some(-magnitude)
    } else {
        Some(magnitude)
    }
}

/// The reading the total's fixed point `fixed` stands for. A mean lies between its readings, each
/// of which `fixed_from_value` kept under 64 bits.
fn value_from_fixed(fixed: i128) -> f64 {
    let magnitude = u64::try_from(fixed.unsigned_abs()).unwrap_or_else(|_| unreachable!());
    let value = crate::rates::number(magnitude) / f64::from(1_u32 << TOTAL_FRACTION_BITS);

    if fixed < 0 { -value } else { value }
}
