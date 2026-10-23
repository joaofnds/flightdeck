//! The recording's model: the App rule over typed columns of one tick and the tick before it,
//! which the store, the persister and a jump share, so each derives a second's Apps the same way.
pub mod apps;
pub mod catalog;
pub mod columns;
pub mod programs;
pub mod ranks;
pub mod rates;

/// Widens `value`, which a `usize` holds on every 64-bit Mac.
#[must_use]
pub fn usize_from_u32(value: u32) -> usize {
    const { assert!(size_of::<u32>() <= size_of::<usize>()) };

    match usize::try_from(value) {
        Ok(widened) => widened,
        Err(error) => unreachable!("{error}"),
    }
}

/// Narrows `value`, a length of a buffer whose capacity a `u32` was widened to.
#[must_use]
pub fn u32_from_length(value: usize) -> u32 {
    match u32::try_from(value) {
        Ok(narrowed) => narrowed,
        Err(error) => unreachable!("{error}"),
    }
}
