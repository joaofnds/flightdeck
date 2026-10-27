//! Process-name capacities and owner-held reservations reached by one published View.
use std::io;

use serde_json::json;
use viewer::store::{ByteBudget, ProcessName, ProcessRow, View};

#[derive(Debug, Default, PartialEq, Eq)]
struct Names {
    references: u64,
    unique: u64,
    raw_capacity_bytes: u64,
    display_capacity_bytes: u64,
    initialized_bytes: u64,
    reserved_bytes: u64,
}

pub(super) fn report(view: &View, budget: &ByteBudget) -> io::Result<()> {
    let mut count = 0_usize;
    visit(view, |_| {
        count = count.checked_add(1).ok_or_else(overflow)?;
        Ok(())
    })?;
    if count > budget.maximum() / size_of::<ProcessRow>() {
        return Err(io::Error::other(
            "name occurrences exceed the payload row bound",
        ));
    }
    let observer_bytes = count
        .checked_mul(size_of::<&ProcessName>())
        .ok_or_else(overflow)?;
    let observer = std::sync::Arc::new(ByteBudget::new(observer_bytes));
    let reservation = observer
        .reserve(observer_bytes)
        .ok_or_else(|| io::Error::other("name observer cannot reserve its records"))?;
    let mut names = Vec::new();
    names.try_reserve_exact(count).map_err(io::Error::other)?;
    if names.capacity() != count {
        return Err(io::Error::other(
            "name observer capacity differs from its bound",
        ));
    }
    visit(view, |name| {
        if names.len() == count {
            return Err(io::Error::other("name observer fill exceeds its count"));
        }
        names.push(name);
        Ok(())
    })?;
    assert_eq!(names.len(), count);
    let mut measured = Names::default();
    measure(&mut names, &mut measured)?;
    let footprint = persistence::resources::Footprint::read()?;
    println!(
        "{}",
        json!({"phase":"published_process_names", "view_version":view.version,
            "name_references":measured.references, "unique_names":measured.unique,
            "raw_capacity_bytes":measured.raw_capacity_bytes,
            "display_capacity_bytes":measured.display_capacity_bytes,
            "initialized_name_bytes":measured.initialized_bytes,
            "unique_value_slot_bytes":measured.unique * u64::try_from(size_of::<ProcessName>())
                .map_err(io::Error::other)?,
            "unique_name_reserved_bytes":measured.reserved_bytes,
            "observer_record_requested_bytes":observer_bytes,
            "observer_record_reserved_bytes":observer.used(),
            "observer_capacity":names.capacity(), "component_only":true,
            "footprint_bytes_with_observer":footprint.current_bytes,
            "footprint_peak_bytes_with_observer":footprint.lifetime_bytes_max,
            "scope":concat!("Names reached by this published View. Capacity totals exclude ",
                "Arc headers; reserved bytes include the Arc/value allowance. Other roots ",
                "and native allocations are excluded. Process footprint is reported ",
                "separately with observer overhead.")}),
    );
    drop(names);
    drop(reservation);
    Ok(())
}

fn visit<'view>(
    view: &'view View,
    mut take: impl FnMut(&'view ProcessName) -> io::Result<()>,
) -> io::Result<()> {
    for apps in &view.apps {
        if let Ok(payload) = &apps.processes {
            for process in payload.iter() {
                if let Some(name) = process.executable_name() {
                    take(name)?;
                }
            }
        }
    }
    Ok(())
}

fn measure(names: &mut [&ProcessName], measured: &mut Names) -> io::Result<()> {
    *measured = Names {
        references: u64::try_from(names.len()).map_err(io::Error::other)?,
        ..Names::default()
    };
    names.sort_unstable_by_key(|name| std::ptr::from_ref(*name).addr());
    let mut previous = None;
    for name in names.iter().copied() {
        let address = std::ptr::from_ref(name).addr();
        if previous == Some(address) {
            continue;
        }
        previous = Some(address);
        add(&mut measured.unique, 1)?;
        add(&mut measured.raw_capacity_bytes, name.bytes.capacity())?;
        add(
            &mut measured.display_capacity_bytes,
            name.display.capacity(),
        )?;
        add(&mut measured.initialized_bytes, name.bytes.len())?;
        add(&mut measured.initialized_bytes, name.display.len())?;
        add(&mut measured.reserved_bytes, name.reserved_bytes())?;
    }
    Ok(())
}

fn add(total: &mut u64, bytes: usize) -> io::Result<()> {
    *total = total
        .checked_add(u64::try_from(bytes).map_err(io::Error::other)?)
        .ok_or_else(overflow)?;
    Ok(())
}

fn overflow() -> io::Error {
    io::Error::other("name census arithmetic overflow")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn shared_names_count_one_backing_across_references() {
        // Goal: aliases do not multiply storage. Method: count two real Arc aliases.
        let name = Arc::new(name());
        let alias = Arc::clone(&name);

        let mut measured = Names::default();
        measure(&mut [&name, &alias], &mut measured).unwrap();

        assert_eq!(
            measured,
            Names {
                references: 2,
                unique: 1,
                raw_capacity_bytes: 3,
                display_capacity_bytes: 3,
                initialized_bytes: 6,
                reserved_bytes: name_bytes(6),
            }
        );
        assert_eq!(Arc::strong_count(&name), 2);
    }

    #[test]
    fn equal_names_keep_independent_backings() {
        // Goal: equal text does not imply sharing. Method: count two independent owners.
        let first = name();
        let second = name();

        let mut measured = Names::default();
        measure(&mut [&first, &second], &mut measured).unwrap();

        assert_eq!(
            measured,
            Names {
                references: 2,
                unique: 2,
                raw_capacity_bytes: 6,
                display_capacity_bytes: 6,
                initialized_bytes: 12,
                reserved_bytes: 2 * name_bytes(6),
            }
        );
    }

    #[test]
    fn unused_name_capacity_is_still_counted() {
        // Goal: empty buffers retain storage. Method: count actual spare capacities.
        let budget = Arc::new(ByteBudget::new(1024));
        let reservation = budget
            .reserve(2 * size_of::<usize>() + size_of::<ProcessName>() + 64 + 128)
            .unwrap();
        let name = ProcessName::from_reserved(
            viewer::live_app_response::Bytes {
                bytes: Vec::with_capacity(64),
                display: String::with_capacity(128),
            },
            reservation,
        );

        let mut measured = Names::default();
        measure(&mut [&name], &mut measured).unwrap();

        assert_eq!(
            measured,
            Names {
                references: 1,
                unique: 1,
                raw_capacity_bytes: 64,
                display_capacity_bytes: 128,
                initialized_bytes: 0,
                reserved_bytes: name_bytes(192),
            }
        );
    }

    fn name() -> ProcessName {
        let budget = Arc::new(ByteBudget::new(1024));
        let reservation = budget
            .reserve(2 * size_of::<usize>() + size_of::<ProcessName>() + 6)
            .unwrap();
        ProcessName::from_reserved(
            viewer::live_app_response::Bytes {
                bytes: b"abc".to_vec(),
                display: "abc".to_owned(),
            },
            reservation,
        )
    }

    fn name_bytes(backing: usize) -> u64 {
        u64::try_from(2 * size_of::<usize>() + size_of::<ProcessName>() + backing).unwrap()
    }
}
