//! One series' readings reduced over a stretch of the clock, so a reading that lasted a second
//! stays findable however far a chart thins the series. A highest keeps the highest reading and
//! the reading it came from, which is all a chart's peak takes. A summary keeps the lowest too,
//! and the summaries of adjacent stretches merge into exactly the summary of their readings,
//! however the readings were cut.

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
};

/// One value of a reduction and the reading it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extreme<Value, Place> {
    pub value: Value,
    pub place: Place,
}

impl<Value: PartialOrd, Place> Extreme<Value, Place> {
    /// The higher of this and `found`, a reading after it. A tie keeps this one, the earlier, so a
    /// held second never moves.
    fn higher(self, found: Self) -> Self {
        if found.value > self.value {
            found
        } else {
            self
        }
    }
}

/// What a series' readings came to: of those with values, the lowest, the highest, the newest and
/// their mean, and how many had none or did not follow the reading before them. A reading of zero
/// is a value. A reading that did not follow marks a break in the line a reader must not draw
/// across.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GaugeSummary<Place> {
    pub measured: Option<Measured<Place>>,
    pub missing: u32,
    pub breaks: u32,
}

/// What a summary found of readings with values: the lowest and the highest, the earliest of any
/// that tie, so a merge finds the one a summary of all of them finds, and the newest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured<Place> {
    pub minimum: Extreme<f64, Place>,
    pub maximum: Extreme<f64, Place>,
    pub last: Extreme<f64, Place>,
    /// How many readings had values, which their mean divides by.
    pub count: u32,
    total: i128,
    overflowed: bool,
}

impl<Place: Copy> GaugeSummary<Place> {
    /// The mean of the readings, or `None` with no reading or where the total outgrew what it
    /// counts. The division truncates a fraction of the total's finest unit.
    #[must_use]
    pub fn mean(&self) -> Option<f64> {
        let measured = self.measured.as_ref().filter(|found| !found.overflowed)?;

        Some(value_from_fixed(
            measured.total / i128::from(measured.count),
        ))
    }

    /// Takes in the reading after the last one. `None` is a reading the collector did not read.
    pub fn push(&mut self, value: Option<f64>, follows: bool, place: Place) {
        if !follows {
            self.breaks += 1;
        }
        let fixed = value.and_then(fixed_from_value);
        let (Some(value), Some(fixed)) = (value, fixed) else {
            self.missing += 1;
            return;
        };

        let mark = Extreme { value, place };
        self.merge_measured(Measured {
            minimum: mark,
            maximum: mark,
            last: mark,
            count: 1,
            total: fixed,
            overflowed: false,
        });
    }

    /// Takes in the summary of the readings right after this one's.
    pub fn merge(&mut self, next: &Self) {
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
        held.maximum = held.maximum.higher(next.maximum);
        held.last = next.last;
        held.count += next.count;
        let total = held.total.checked_add(next.total);
        held.total = total.unwrap_or(0);
        held.overflowed = held.overflowed || next.overflowed || total.is_none();
    }
}

/// The highest of a series' readings and the reading it came from, the earliest of any that tie.
/// It keeps no mean, lowest or count, for a reader that takes only the highest reading and its
/// place, as a chart's peak does.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Highest<Place> {
    highest: Option<Extreme<f64, Place>>,
}

impl<Place: Copy> Highest<Place> {
    /// The highest reading taken in, or `None` before a reading with a value.
    #[must_use]
    pub fn highest(&self) -> Option<&Extreme<f64, Place>> {
        self.highest.as_ref()
    }

    /// Takes in the reading after the last one. `None` is a reading the collector did not read. A
    /// reading `fixed_from_value` refuses is no quantity the Mac reports, so, as in a summary, it
    /// never becomes the highest.
    pub fn push(&mut self, value: Option<f64>, place: Place) {
        let Some(value) = value.filter(|value| fixed_from_value(*value).is_some()) else {
            return;
        };

        let found = Extreme { value, place };
        self.highest = Some(self.highest.map_or(found, |held| held.higher(found)));
    }
}

/// The highest of a counter's rates. A rate over no elapsed time is no rate, so it never becomes
/// the highest.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RateHighest<Place>(Highest<Place>);

impl<Place: Copy> RateHighest<Place> {
    /// Takes in the rate over the `elapsed_ns` after the last reading, or `None` where the two
    /// readings give no rate.
    pub fn push(&mut self, rate: Option<(f64, u64)>, place: Place) {
        let value = rate
            .filter(|(_rate, elapsed_ns)| *elapsed_ns > 0)
            .map(|(rate, _elapsed_ns)| rate);
        self.0.push(value, place);
    }
}

impl<Place> std::ops::Deref for RateHighest<Place> {
    type Target = Highest<Place>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The highest category of a series' readings and the reading that first showed it, which is all
/// a reader of a state's peak takes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CategoryHighest<Category, Place> {
    highest: Option<Extreme<Category, Place>>,
}

impl<Category: Copy + Ord, Place: Copy> CategoryHighest<Category, Place> {
    /// The highest category taken in, or `None` before a reading that read one.
    #[must_use]
    pub fn highest(&self) -> Option<&Extreme<Category, Place>> {
        self.highest.as_ref()
    }

    /// Takes in the reading after the last one. `None` is a reading the collector did not read.
    pub fn push(&mut self, category: Option<Category>, place: Place) {
        let Some(value) = category else {
            return;
        };

        let found = Extreme { value, place };
        self.highest = Some(self.highest.map_or(found, |held| held.higher(found)));
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
