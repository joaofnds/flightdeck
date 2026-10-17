use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::ring::{DictionaryCapacity, RingReader};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Harness {
    root: PathBuf,
    place: Place,
}

impl Harness {
    fn new() -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "fd-identity-registration-{}-{number}",
            std::process::id()
        ));
        create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdri{}.{number}.", std::process::id()),
        };

        Self { root, place }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        match std::fs::read_to_string(&self.place.lock_path) {
            Ok(name) => crate::ring::unlink_generation(&name).unwrap(),
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
        }

        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn shape() -> Shape {
    Shape {
        slot_count: 2,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 2,
        coalition_capacity: 1,
        event_capacity: 2,
    }
}

fn capacity() -> DictionaryCapacity {
    DictionaryCapacity {
        entries: 4,
        arena_bytes: 128,
    }
}

#[test]
fn registers_an_identity_ring_without_reacquiring_its_place_lock() {
    // Goal: identity registration retains the single lifetime lock through creation.
    // Method: start normally and inspect the actual registered reader and dictionary.
    let harness = Harness::new();

    let (writer, registered) =
        Registration::start_with_identity(&harness.root, &harness.place, shape(), capacity(), 71)
            .unwrap()
            .into_parts();
    registered.unwrap();
    let reader = RingReader::open_generation(writer.name()).unwrap();

    assert_eq!(reader.source_layout_version(), 4);
    assert_eq!(
        reader.dictionary_capacity().unwrap().entries,
        capacity().entries
    );
    assert_eq!(
        reader.dictionary_capacity().unwrap().arena_bytes,
        capacity().arena_bytes
    );
    assert_eq!(reader.identity(), writer.identity());
    assert_eq!(reader.run_start_continuous_ns(), Some(71));
    assert_eq!(
        Registry::read(&harness.root).unwrap().runs[0].ring_name,
        writer.name()
    );
}

#[test]
fn refuses_a_second_identity_collector_before_mutating_either_registry() {
    // Goal: a new identity registration cannot bypass an existing lifetime ring lock.
    // Method: keep the first registered writer while offering the same place to another root.
    let first = Harness::new();
    let second = Harness::new();
    let (writer, registered) =
        Registration::start_with_identity(&first.root, &first.place, shape(), capacity(), 1)
            .unwrap()
            .into_parts();
    registered.unwrap();
    let before = std::fs::read(first.root.join("runs")).unwrap();

    let error =
        Registration::start_with_identity(&second.root, &first.place, shape(), capacity(), 2)
            .err()
            .unwrap();

    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(std::fs::read(first.root.join("runs")).unwrap(), before);
    assert!(!second.root.join("runs").exists());
    assert_eq!(
        RingReader::open_generation(writer.name())
            .unwrap()
            .source_layout_version(),
        4
    );
}

#[test]
fn keeps_registration_refusal_on_the_existing_unregistered_v3_fallback() {
    // Goal: unavailable private storage cannot introduce unregistered identity-format writes.
    // Method: refuse privacy validation and inspect the resulting anonymous source.
    use std::os::unix::fs::PermissionsExt;
    let harness = Harness::new();
    std::fs::set_permissions(&harness.root, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (writer, registered) =
        Registration::start_with_identity(&harness.root, &harness.place, shape(), capacity(), 71)
            .unwrap()
            .into_parts();
    let reader = RingReader::open_generation(writer.name()).unwrap();

    assert_eq!(
        registered.unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(reader.source_layout_version(), 3);
    assert!(reader.dictionary_capacity().is_none());
    assert!(reader.identity().is_none());
    assert_eq!(reader.run_start_continuous_ns(), Some(71));
}

#[test]
fn keeps_the_compatibility_registration_entry_point_on_v3() {
    // Goal: retained compatibility fixtures keep their original legacy writer constructor.
    // Method: register through start rather than the explicit identity entry point.
    let harness = Harness::new();

    let (writer, registered) = Registration::start(&harness.root, &harness.place, shape(), 71)
        .unwrap()
        .into_parts();
    registered.unwrap();
    let reader = RingReader::open_generation(writer.name()).unwrap();

    assert_eq!(reader.source_layout_version(), 3);
    assert!(reader.dictionary_capacity().is_none());
    assert_eq!(reader.identity(), writer.identity());
}

#[test]
fn plans_the_full_profile_below_the_existing_one_gibibyte_reader_limit() {
    // Goal: activation uses the exact current layout rather than an inline-size estimate.
    // Method: plan the documented hardware ceiling without allocating its mapping.
    let shape = Shape {
        slot_count: 120,
        cpu_capacity: 18,
        interface_capacity: 1638,
        core_capacity: 18,
        state_capacity: 64,
        fan_capacity: 10,
        process_capacity: 12_000,
        coalition_capacity: 28_384,
        event_capacity: 1_966_080,
    };
    let capacity = DictionaryCapacity {
        entries: 65_536,
        arena_bytes: 64 * 1024 * 1024,
    };

    let bytes = Writer::identity_size_bytes(shape, capacity).unwrap();

    assert_eq!(bytes, 1_012_654_976);
    assert_eq!(1024 * 1024 * 1024 - bytes, 61_086_848);
}

#[test]
fn refuses_an_oversized_identity_mapping_before_creating_registration_artifacts() {
    // Goal: a source the live reader cannot map fails explicitly before registration.
    // Method: offer twice the full profile's slot count and inspect untouched private state.
    let harness = Harness::new();
    let shape = Shape {
        slot_count: 240,
        cpu_capacity: 18,
        interface_capacity: 1638,
        core_capacity: 18,
        state_capacity: 64,
        fan_capacity: 10,
        process_capacity: 12_000,
        coalition_capacity: 28_384,
        event_capacity: 1_966_080,
    };
    let capacity = DictionaryCapacity {
        entries: 65_536,
        arena_bytes: 64 * 1024 * 1024,
    };

    let error =
        Registration::start_with_identity(&harness.root, &harness.place, shape, capacity, 1)
            .err()
            .unwrap();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(
        error
            .to_string()
            .contains("above the 1073741824-byte limit")
    );
    assert!(!harness.root.join("identity").exists());
    assert!(!harness.root.join("runs").exists());
    assert!(!harness.place.lock_path.exists());
}
