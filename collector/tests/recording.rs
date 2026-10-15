//! Recording registration respects the ownership carried by a retained ring.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use collector::recording::{Registry, private};
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
            .capture_previous(&ring.place.lock_path, Identity([99, 100]))
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
