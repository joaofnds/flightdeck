use super::super::stretch_tests::{
    regular, regular_on_the_gpu, stretch_of_one_app, stretches_with_a_load_added,
};
use super::{AMOUNT_COUNT, Amount, Amounts, AppsStretch, RESIDENT_PLACE, rate_place};
use crate::live_app_response::Metric;

/// Sets of amounts the packing property is checked over.
const DRAWS: usize = 20_000;

#[test]
fn amounts_give_back_what_they_keep() {
    // Goal: amounts answer every reader from exactly what they were given, whichever form they
    // keep it in. Method: one value keeps in turn sets drawn by a generator of fixed seed, half
    // of them repeating one another as nearly every App's do and half changed in one field of
    // one amount, so it moves between keeping them once and keeping them whole, and gives back
    // each set it kept, one amount at a time and all of them.
    let mut kept = Amounts::default();
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut kept_alike = 0;

    for _ in 0..DRAWS {
        let amounts = drawn(&mut random);

        kept.keep(amounts);

        let read: [Amount; AMOUNT_COUNT] = std::array::from_fn(|place| kept.amount(place));
        assert_eq!(read, amounts, "kept {amounts:?}");
        assert_eq!(kept.unpacked(), amounts, "kept {amounts:?}");
        if matches!(kept, Amounts::Alike(_)) {
            kept_alike += 1;
        }
    }
    // The generator reached both forms.
    assert!(kept_alike > DRAWS / 4);
    assert!(kept_alike < DRAWS * 3 / 4);
}

#[test]
fn amounts_that_repeat_one_another_hold_no_box() {
    // Goal: the amounts of nearly every App cost no allocation of their own. Method: keep three
    // sets, those of an App whose members measured every rate and its memory at every second
    // with a GPU that measured nothing, as on this Mac, the same with a GPU that measured too,
    // and those of nothing at all.
    let gpu_unmeasured = Amount {
        unavailable_members: 3,
        ..Amount::default()
    };
    let sets = [
        repeating(gpu_unmeasured),
        repeating(rate(4_000)),
        [Amount::default(); AMOUNT_COUNT],
    ];

    let kept = sets.map(|amounts| {
        let mut kept = Amounts::default();
        kept.keep(amounts);
        matches!(kept, Amounts::Alike(_))
    });

    assert_eq!(kept, [true; 3]);
}

#[test]
fn new_amounts_give_back_no_amount() {
    // Goal: a row starts from nothing of any resource. Method: read the amounts a row starts
    // with.
    assert_eq!(
        Amounts::default().unpacked(),
        [Amount::default(); AMOUNT_COUNT]
    );
}

#[test]
fn a_row_the_store_adds_up_holds_no_box() {
    // Goal: the store's own adding keeps the row of a regular App once, so the rows cost no
    // allocation of their own. Method: a stretch adds up ten seconds of an App that uses each
    // resource but the GPU, as on this Mac, and its row's form is read.
    let stretch = stretch_of_one_app(&regular(b"/A.app"));

    assert_eq!(rows_alike(&stretch), [true]);
}

#[test]
fn a_row_the_store_adds_up_of_an_app_on_the_gpu_holds_no_box() {
    // Goal: on a Mac whose GPU measures its Apps, the store's own adding keeps a regular App's
    // row once too. Method: a stretch adds up ten seconds of an App that uses the GPU as well,
    // and its row's form is read.
    let stretch = stretch_of_one_app(&regular_on_the_gpu(b"/A.app"));

    assert_eq!(rows_alike(&stretch), [true]);
}

#[test]
fn a_row_added_up_from_two_stretches_holds_no_box() {
    // Goal: a row a load adds to the row of the same stretch stays kept once. Method: the store
    // takes in five seconds of A and adds a load's stretch of five more of A and of B, and the
    // forms of both rows are read.
    let stretches = stretches_with_a_load_added();

    let stretch = stretches.held().next().unwrap();

    assert_eq!(rows_alike(stretch), [true, true]);
}

/// Whether each row of `stretch` keeps its amounts once, in key order.
fn rows_alike(stretch: &AppsStretch) -> Vec<bool> {
    let rows = stretch.apps().unwrap();
    rows.iter()
        .map(|row| matches!(row.amounts, Amounts::Alike(_)))
        .collect()
}

/// The amounts of an App of three members that measured each rate over ten seconds, and its
/// memory at each, with `gpu` its amount of the GPU.
fn repeating(gpu: Amount) -> [Amount; AMOUNT_COUNT] {
    let mut amounts = [1_000, 2_000, 3_000, 0].map(rate);
    amounts[rate_place(Metric::Gpu)] = gpu;
    let resident = Amount {
        measured_ns: 0,
        ..rate(40_960)
    };

    [amounts[0], amounts[1], amounts[2], amounts[3], resident]
}

/// What three members that measured a rate over ten seconds used of it, `known`.
fn rate(known: u64) -> Amount {
    Amount {
        known,
        measured_ns: 10_000_000_000,
        measured_seconds: 10,
        measured_members: 3,
        unavailable_members: 0,
        overflowed: false,
    }
}

/// A set of amounts as `Random` draws them: amounts that repeat one another as a regular App's
/// do, with a GPU that measured or one that did not, and in half of the draws one field of one
/// of them changed.
fn drawn(random: &mut Random) -> [Amount; AMOUNT_COUNT] {
    let shared = Amount {
        known: 0,
        measured_ns: random.below(3) * 1_000_000_000,
        measured_seconds: random.count(),
        measured_members: random.count(),
        unavailable_members: 0,
        overflowed: false,
    };
    let mut amounts = [shared; AMOUNT_COUNT];
    for amount in &mut amounts {
        amount.known = random.below(4) * 1_000;
    }
    amounts[RESIDENT_PLACE].measured_ns = 0;
    if random.below(2) == 0 {
        amounts[rate_place(Metric::Gpu)] = Amount {
            unavailable_members: shared.measured_members,
            ..Amount::default()
        };
    }

    if random.below(2) == 0 {
        let place = usize::try_from(random.below(5)).unwrap();
        changed(&mut amounts[place], random);
    }

    amounts
}

/// Changes one field of `amount`, which one `random` draws.
fn changed(amount: &mut Amount, random: &mut Random) {
    match random.below(6) {
        0 => amount.known += 1,
        1 => amount.measured_ns += 1,
        2 => amount.measured_seconds += 1,
        3 => amount.measured_members += 1,
        4 => amount.unavailable_members += 1,
        _ => amount.overflowed = !amount.overflowed,
    }
}

/// A generator of fixed seed, xorshift64, so the property draws the same amounts on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number below `bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// A count of seconds or members: none, a few, or past what 16 bits count.
    fn count(&mut self) -> u32 {
        [0, 1, 10, 8_000, 70_000][usize::try_from(self.below(5)).unwrap()]
    }
}
