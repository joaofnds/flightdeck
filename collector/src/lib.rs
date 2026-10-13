pub mod clock;
pub mod coalitions;
pub mod collector;
pub mod cost;
pub mod interfaces;
pub mod ioreport;
mod kernel;
pub mod machine;
pub mod processes;
pub mod schedule;
pub mod smc;
pub mod tracker;

/// Pushes `item` onto `list`, whose room was taken at startup for every item it holds, so
/// the push never grows it.
pub(crate) fn push_without_growing<T>(list: &mut Vec<T>, item: T) {
    let capacity = list.capacity();
    assert!(list.len() < capacity);

    list.push(item);
    assert_eq!(capacity, list.capacity());
}
