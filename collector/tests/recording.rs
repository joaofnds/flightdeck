//! Recording registration respects the ownership carried by a retained ring.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use collector::recording::{Registration, Registry, private};
    use collector::ring::{Identity, Place, RingReader, RunIdentity, Shape, Writer};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    struct RetainedRing {
        root: PathBuf,
        place: Place,
        writer: Writer,
    }

    #[test]
    fn leaves_another_recordings_ring_out_of_its_registry() {
        // Goal: changing recording destination cannot adopt or later unlink foreign history.
        // Method: capture a real retained ring into a registry for another recording.
        let ring = RetainedRing::new();
        let mut registry = Registry::default();

        registry
            .capture_previous(&ring.root, &ring.place, Identity([99, 100]))
            .unwrap();

        assert!(registry.runs.is_empty());
        assert_eq!(registry.evictions, 0);
        assert_eq!(
            RingReader::open_generation(ring.writer.name())
                .unwrap()
                .identity(),
            ring.writer.identity()
        );
    }

    #[test]
    fn one_recording_keeps_one_live_collector_across_distinct_ring_places() {
        // Goal: distinct ring names cannot register concurrent collectors in one recording.
        // Method: keep the first writer alive while a second startup uses another place.
        let ring = RetainedRing::new();
        let first_place = place(&ring.root, "first");
        let second_place = place(&ring.root, "second");
        let first = Registration::start(&ring.root, &first_place, shape(), 0).unwrap();
        let (first, registered) = first.into_parts();
        registered.unwrap();
        let first = OwnedWriter(first);

        let second = Registration::start(&ring.root, &second_place, shape(), 0).unwrap();
        let (second, registered) = second.into_parts();
        let second = OwnedWriter(second);

        assert_eq!(
            std::io::ErrorKind::WouldBlock,
            registered.unwrap_err().kind()
        );
        let registry = Registry::read(&ring.root).unwrap();
        assert_eq!(1, registry.runs.len());
        assert_eq!(first.0.name(), registry.runs[0].ring_name);
        assert_eq!(None, second.0.identity());
    }

    #[test]
    fn a_live_place_refuses_another_recording_before_either_registry_changes() {
        // Goal: recording ownership cannot bypass the lifetime lock of a shared ring place.
        // Method: start two recordings at one live place and compare both registry inodes.
        use std::os::unix::fs::MetadataExt;
        let ring = RetainedRing::new();
        let place = place(&ring.root, "shared-place");
        let (first, status) = Registration::start(&ring.root, &place, shape(), 0)
            .unwrap()
            .into_parts();
        let first = OwnedWriter(first);
        status.unwrap();
        let other = ring.root.join("other-recording");
        private::create_private_dir(&other).unwrap();
        collector::recording::recording_identity(&other).unwrap();
        Registry::default().write(&other).unwrap();
        let first_before = std::fs::metadata(ring.root.join("runs")).unwrap().ino();
        let other_before = std::fs::metadata(other.join("runs")).unwrap().ino();

        let refused = Registration::start(&other, &place, shape(), 0)
            .err()
            .unwrap();

        assert_eq!(std::io::ErrorKind::WouldBlock, refused.kind());
        assert_eq!(
            first_before,
            std::fs::metadata(ring.root.join("runs")).unwrap().ino()
        );
        assert_eq!(
            other_before,
            std::fs::metadata(other.join("runs")).unwrap().ino()
        );
        assert_eq!(
            first.0.name(),
            Registry::read(&ring.root).unwrap().runs[0].ring_name
        );
        assert!(Registry::read(&other).unwrap().runs.is_empty());
    }

    #[test]
    fn missing_identity_refuses_an_existing_recording_without_assigning_a_new_one() {
        // Goal: deleting identity metadata cannot silently assign saved history a new owner.
        // Method: offer each recording artifact without identity and inspect the refusal.
        let ring = RetainedRing::new();
        for name in ["data", "commits", "runs", "retired"] {
            let artifact = ring.root.join(name);
            private::create_private_dir(&artifact).unwrap();

            let refused = collector::recording::recording_identity(&ring.root);

            let identity_exists = ring.root.join("identity").exists();
            std::fs::remove_dir(&artifact).unwrap();
            assert_eq!(std::io::ErrorKind::InvalidData, refused.unwrap_err().kind());
            assert!(!identity_exists);
        }
    }

    #[test]
    fn waits_for_a_short_registration_transaction_before_starting_recording() {
        // Goal: ordinary overlap with a persister registry update does not disable recording.
        // Method: hold the short lock for 100 ms, then observe successful ordered startup.
        let ring = RetainedRing::new();
        let lock = private::FileLock::try_lock(&ring.root.join("registration.lock")).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(lock);
        });

        let started =
            Registration::start(&ring.root, &place(&ring.root, "wait"), shape(), 31).unwrap();
        let (writer, registered) = started.into_parts();
        let writer = OwnedWriter(writer);
        release.join().unwrap();

        registered.unwrap();
        assert_eq!(1, Registry::read(&ring.root).unwrap().runs.len());
        assert_eq!(
            Some(31),
            RingReader::open_generation(writer.0.name())
                .unwrap()
                .run_start_continuous_ns()
        );
    }

    #[test]
    fn empty_and_foreign_lock_names_use_normal_ring_startup_rules() {
        // Goal: recovery cannot reject lock contents that normal ring creation ignores.
        // Method: start from empty and foreign-prefix lock files and retain both old data.
        let ring = RetainedRing::new();
        for (label, bytes) in [
            ("empty", &b""[..]),
            ("foreign", &b"/other.abcdefghijkl.7"[..]),
        ] {
            let place = place(&ring.root, label);
            let mut lock = private::open_private_file(&place.lock_path, true).unwrap();
            std::io::Write::write_all(&mut lock, bytes).unwrap();

            let (writer, status) = Registration::start(&ring.root, &place, shape(), 0)
                .unwrap()
                .into_parts();
            let writer = OwnedWriter(writer);

            status.unwrap();
            assert!(writer.0.identity().is_some());
        }
    }

    #[test]
    fn repeated_storage_refusal_bounds_anonymous_rings_and_preserves_actual_origin() {
        // Goal: failed destination setup retains two anonymous generations in bounded work.
        // Method: repeatedly start against a file, checking old names and measured origin.
        let ring = RetainedRing::new();
        let refused = ring.root.join("not-a-directory");
        drop(private::open_private_file(&refused, true).unwrap());
        let place = place(&ring.root, "fallback");
        let mut names = Vec::new();
        for start in 1..=6 {
            let (writer, status) = Registration::start(&refused, &place, shape(), start)
                .unwrap()
                .into_parts();
            assert!(status.is_err());
            assert_eq!(None, writer.identity());
            assert_eq!(
                Some(start),
                RingReader::open_generation(writer.name())
                    .unwrap()
                    .run_start_continuous_ns()
            );
            names.push(writer.name().to_owned());
            drop(writer);
        }

        for (index, name) in names.iter().enumerate() {
            if index + 2 < names.len() {
                assert_eq!(
                    std::io::ErrorKind::NotFound,
                    RingReader::open_generation(name).err().unwrap().kind()
                );
            } else {
                assert!(RingReader::open_generation(name).is_ok());
                collector::ring::unlink_generation(name).unwrap();
            }
        }
    }

    #[test]
    fn a_large_generation_number_does_not_expand_refused_storage_startup_work() {
        // Goal: a same-user lock cannot make fallback scan the whole generation space.
        // Method: start after a canonical 14-digit generation against a refused root.
        let ring = RetainedRing::new();
        let refused = ring.root.join("huge-not-a-directory");
        drop(private::open_private_file(&refused, true).unwrap());
        let place = place(&ring.root, "huge");
        let mut lock = private::open_private_file(&place.lock_path, true).unwrap();
        std::io::Write::write_all(&mut lock, b"/fd.abcdefghijkl.99999999999998").unwrap();
        let start = std::time::Instant::now();

        let (writer, status) = Registration::start(&refused, &place, shape(), 71)
            .unwrap()
            .into_parts();
        let writer = OwnedWriter(writer);

        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert!(status.is_err());
        assert_eq!(99_999_999_999_999, writer.0.generation());
    }

    #[test]
    fn failed_registration_keeps_recording_ownership_until_its_writer_drops() {
        // Goal: a registration refusal cannot release ownership while sampling continues.
        // Method: refuse the short lock, release it, and try two more collector starts.
        let ring = RetainedRing::new();
        let short = private::FileLock::try_lock(&ring.root.join("registration.lock")).unwrap();
        let first =
            Registration::start(&ring.root, &place(&ring.root, "refused"), shape(), 0).unwrap();
        let (first, status) = first.into_parts();
        let first = OwnedWriter(first);
        assert_eq!(std::io::ErrorKind::WouldBlock, status.unwrap_err().kind());
        drop(short);

        let second =
            Registration::start(&ring.root, &place(&ring.root, "blocked"), shape(), 0).unwrap();
        let (second, status) = second.into_parts();
        let second = OwnedWriter(second);
        assert_eq!(std::io::ErrorKind::WouldBlock, status.unwrap_err().kind());
        assert!(Registry::read(&ring.root).unwrap().runs.is_empty());
        drop(first);

        let third =
            Registration::start(&ring.root, &place(&ring.root, "released"), shape(), 0).unwrap();
        let (third, status) = third.into_parts();
        let third = OwnedWriter(third);
        status.unwrap();
        drop(third);
        drop(second);
    }

    #[test]
    fn replaces_an_interrupted_identity_temporary_without_publishing_partial_identity() {
        // Goal: interruption during first identity initialization cannot poison future starts.
        // Method: leave a partial private pending file, then initialize and read the identity.
        let ring = RetainedRing::new();
        let pending = ring.root.join("identity.pending");
        let mut file = private::open_private_file(&pending, true).unwrap();
        std::io::Write::write_all(&mut file, b"00000000000").unwrap();

        let identity = collector::recording::recording_identity(&ring.root).unwrap();

        assert_eq!(
            33,
            std::fs::metadata(ring.root.join("identity")).unwrap().len()
        );
        assert!(!pending.exists());
        assert_eq!(
            identity,
            collector::recording::recording_identity(&ring.root).unwrap()
        );
    }

    #[test]
    fn refuses_an_identity_with_more_than_one_trailing_newline() {
        // Goal: collector and disk readers accept the same exact identity representation.
        // Method: present a valid hex identity followed by two line endings.
        let ring = RetainedRing::new();
        let mut file = private::open_private_file(&ring.root.join("identity"), true).unwrap();
        std::io::Write::write_all(&mut file, b"00000000000000010000000000000002\n\n").unwrap();

        let read = collector::recording::recording_identity(&ring.root);

        assert_eq!(std::io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    struct OwnedWriter(Writer);

    impl Drop for OwnedWriter {
        fn drop(&mut self) {
            collector::ring::unlink_generation(self.0.name()).unwrap();
        }
    }

    fn place(root: &std::path::Path, label: &str) -> Place {
        Place {
            lock_path: root.join(format!("{label}.lock")),
            name_prefix: "/fd.".to_owned(),
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
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 2,
        }
    }

    impl RetainedRing {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "flightdeck-registration-owner-{}-{serial}",
                std::process::id()
            ));
            private::create_private_dir(&root).unwrap();
            let place = Place {
                lock_path: root.join("ring.lock"),
                name_prefix: format!("/fdo{}{serial}", std::process::id()),
            };
            let identity = RunIdentity {
                recording: Identity([1, 2]),
                run: Identity([3, 4]),
                boot: Identity([5, 6]),
            };
            let shape = Shape {
                slot_count: 2,
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 2,
            };
            let writer = Writer::create_recording(&place, shape, &identity, 0).unwrap();
            Self {
                root,
                place,
                writer,
            }
        }
    }

    impl Drop for RetainedRing {
        fn drop(&mut self) {
            collector::ring::unlink_generation(self.writer.name()).unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
}
