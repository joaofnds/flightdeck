//! Rows of one App each, held in the key order of their Apps, into which the Apps of a point
//! are added: a stretch adding up a second's Apps, and a chart's leaders adding up each point's.

use super::attribution::PointRow;
use crate::apps::AppKey;
use crate::live_app_response::RankedApp;

#[cfg(test)]
#[path = "key_order_tests.rs"]
mod tests;

/// A row of one App that rows held in key order hold, which the Apps of `R` add to.
pub(super) trait HeldRow<R> {
    /// The App the row holds.
    fn name(&self) -> &RankedApp;
    /// A row of the App `row` holds, with nothing added to it yet.
    fn starting(row: &R) -> Self;
}

/// Writes into `merged` the rows of `held` with those of `incoming` added with `add`, in key
/// order. Returns false where they come to more than `apps_max` Apps.
pub(super) fn add_merged<H: HeldRow<R>, R: PointRow>(
    held: &mut Vec<H>,
    incoming: &[R],
    merged: &mut Vec<H>,
    apps_max: usize,
    add: impl Fn(&mut H, &R),
) -> bool {
    assert!(merged.is_empty());

    let mut held = held.drain(..).peekable();
    for app in incoming {
        let key = key_of(app.name());
        while let Some(row) = held.next_if(|row| key_of(row.name()) < key) {
            if merged.len() == apps_max {
                return false;
            }
            merged.push(row);
        }
        // An App out of key order would take a second row of its own.
        if let Some(last) = merged.last() {
            assert!(key_of(last.name()) < key);
        }
        let mut row = match held.next_if(|row| key_of(row.name()) == key) {
            Some(row) => row,
            None => H::starting(app),
        };

        add(&mut row, app);
        if merged.len() == apps_max {
            return false;
        }
        merged.push(row);
    }
    for row in held {
        if merged.len() == apps_max {
            return false;
        }
        merged.push(row);
    }

    true
}

/// The key that orders the App `name` among the others.
pub(super) fn key_of(name: &RankedApp) -> AppKey<'_> {
    name.kind.key(&name.bytes)
}
