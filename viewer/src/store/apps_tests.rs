use collector::ring::{Identity, RunIdentity};

use super::{AppsSecond, Kept, Missing};
use crate::live_app_response::{KeyKind, Measure};
use crate::recording::{AppMinute, AppRow, SampleRow};

const RUN: RunIdentity = RunIdentity {
    recording: Identity([1, 1]),
    run: Identity([2, 2]),
    boot: Identity([3, 3]),
};

/// A measure one member measured, whose amount the tests read nothing of.
const MEASURED: Measure = Measure {
    known: 0,
    measured_members: 1,
    unavailable_members: 0,
    overflowed: false,
};

mod keep {
    use super::{Missing, kept_second};

    #[test]
    fn holds_each_app_its_sealed_minute_keeps_in_key_order() {
        // Goal: a second a loader read holds the Apps its sealed minute keeps. Method: keep a
        // minute whose observation names A and B in key order.
        let second = kept_second(&[b"/A.app", b"/B.app"]);

        assert_eq!(second.missing, None);
        assert_eq!(names_of(&second), ["/A.app", "/B.app"]);
    }

    mod when_the_minute_names_an_app_twice {
        use super::{Missing, kept_second};

        #[test]
        fn holds_none_of_its_apps() {
            // Goal: a minute read from disk that names an App twice gives its second no Apps,
            // since the store finds each App of a second by its key. Method: keep a minute
            // whose observation names A twice.
            let second = kept_second(&[b"/A.app", b"/A.app"]);

            assert!(matches!(second.missing, Some(Missing::Refused(_))));
            assert!(second.apps.is_empty());
        }
    }

    mod when_the_minute_names_its_apps_out_of_key_order {
        use super::{Missing, kept_second};

        #[test]
        fn holds_none_of_its_apps() {
            // Goal: a minute read from disk that names its Apps out of key order gives its
            // second no Apps, since the store finds each App of a second by its key. Method:
            // keep a minute whose observation names B before A.
            let second = kept_second(&[b"/B.app", b"/A.app"]);

            assert!(matches!(second.missing, Some(Missing::Refused(_))));
            assert!(second.apps.is_empty());
        }
    }

    fn names_of(second: &super::AppsSecond) -> Vec<String> {
        second
            .apps
            .iter()
            .map(|row| String::from_utf8(row.name.bytes.clone()).unwrap())
            .collect()
    }
}

/// The second `AppsSecond::keep` makes of a minute whose one observation names the bundles
/// `keys`, in that order.
fn kept_second(keys: &[&[u8]]) -> AppsSecond {
    let mut minute = AppMinute::default();
    for key in keys {
        let key_offset = minute.keys.len();
        minute.keys.extend_from_slice(key);
        minute.apps.push(AppRow {
            publication: 1,
            kind: KeyKind::Bundle,
            key_offset,
            key_size: key.len(),
            members: 1,
            live_members: 1,
            membership_complete: true,
            cpu_ns: MEASURED,
            energy_nj: MEASURED,
            disk_read_bytes: MEASURED,
            disk_written_bytes: MEASURED,
            disk_io_bytes: MEASURED,
            resident_bytes: MEASURED,
            footprint_bytes: MEASURED,
            gpu_ns: MEASURED,
        });
    }
    let count = u64::try_from(keys.len()).unwrap();
    let sample = SampleRow {
        publication: 1,
        run_tick: 1,
        wall_ns: Some(1_001_000_000_000),
        continuous_ns: Some(1_000_000_000),
        awake_ns: Some(1_000_000_000),
        previous: None,
        elapsed_ns: None,
        membership_complete: true,
        ungrouped_members: 0,
        live_ungrouped_members: 0,
        usage_refused: 0,
        usage_gone: 0,
        app_count: count,
        process_count: count,
    };
    let mut second = AppsSecond::with_room();

    second.keep(
        &Kept {
            run: &RUN,
            sample: &sample,
            rows: &minute.apps,
            minute: &minute,
        },
        None,
    );
    second
}
