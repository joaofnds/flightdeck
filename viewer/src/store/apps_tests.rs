use collector::ring::{Identity, RunIdentity};

use super::{AppsSecond, Kept, Missing, TickApps};
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

mod take {
    use super::{AppsSecond, TickApps, kept_second, loaded_second};

    #[test]
    fn keeps_the_buffer_the_store_made_for_the_second() {
        // Goal: a second a loader read moves into the buffer the store made for the second when
        // it started, so the store's buffers stay the ones it allocated then, and keep resident
        // only the pages its Apps were written to. Method: take a loaded second with room for
        // its one App into a buffer with room for every App, and compare the buffer's address
        // before and after.
        let mut held = AppsSecond::with_room();
        let room = held.apps.as_ptr();
        let mut loaded = loaded_second(&[b"/A.app"]);

        held.take(TickApps::Loaded(&mut loaded), None, None);

        assert_eq!(held.apps.as_ptr(), room);
    }

    #[test]
    fn holds_what_the_loader_read_of_the_second() {
        // Goal: a second a loader read holds, once the store takes it in, the Apps and the
        // sample the loader read of it. Method: take a loaded second naming A and B into a
        // buffer with room for every App, and compare it with the second the loader made.
        let mut held = AppsSecond::with_room();
        let mut loaded = loaded_second(&[b"/A.app", b"/B.app"]);

        held.take(TickApps::Loaded(&mut loaded), None, None);

        assert_eq!(held, kept_second(&[b"/A.app", b"/B.app"]));
    }
}

mod attribution {
    use std::sync::Arc;

    use super::{SampleRow, kept_second_of, sample_naming};
    use crate::AppAttributionMetric;
    use crate::live_app_response::Measure;
    use crate::store::{AttributionReading, OtherReading};

    /// What the Apps of a second whose readings found every process used, where it holds none.
    const NOTHING: Measure = Measure {
        known: 0,
        measured_members: 0,
        unavailable_members: 0,
        overflowed: false,
    };

    #[test]
    fn shows_the_other_apps_using_nothing_where_a_chart_draws_every_app() {
        // Goal: a chart that draws every App of a second whose readings found every process
        // shows the other Apps using nothing there, as the worker shows them. Method: keep a
        // minute whose one observation, measured over a second, names only A, and read the CPU
        // chart that draws A.
        let second = kept_second_of(&[b"/A.app"], &measured());
        let cohort = [Arc::clone(&second.apps[0].name)];

        let shown = second.attribution(AppAttributionMetric::Cpu, &cohort);

        assert_eq!(
            shown.other,
            Some(OtherReading {
                app_count: 0,
                reading: AttributionReading {
                    rate: Some(0.0),
                    measure: Some(NOTHING),
                    membership_complete: true,
                },
            })
        );
    }

    mod when_the_readings_missed_a_process {
        use std::sync::Arc;

        use super::{AttributionReading, OtherReading, SampleRow, kept_second_of, measured};
        use crate::AppAttributionMetric;

        #[test]
        fn shows_the_other_apps_unknown_where_a_chart_draws_every_app() {
            // Goal: a chart that draws every App of a second whose readings placed a process in
            // no App shows the other Apps unknown, since that process may belong to an App the
            // second missed, as the worker shows them. Method: as where the readings found every
            // process, with one process the App rule placed in no App.
            let sample = SampleRow {
                ungrouped_members: 1,
                ..measured()
            };
            let second = kept_second_of(&[b"/A.app"], &sample);
            let cohort = [Arc::clone(&second.apps[0].name)];

            let shown = second.attribution(AppAttributionMetric::Cpu, &cohort);

            assert_eq!(
                shown.other,
                Some(OtherReading {
                    app_count: 0,
                    reading: AttributionReading {
                        rate: None,
                        measure: None,
                        membership_complete: true,
                    },
                })
            );
        }
    }

    /// The sample of an observation of A measured over a second.
    fn measured() -> SampleRow {
        SampleRow {
            elapsed_ns: Some(1_000_000_000),
            ..sample_naming(&[b"/A.app"])
        }
    }
}

/// The second `AppsSecond::keep` makes of a minute whose one observation names the bundles
/// `keys`, in that order, and measured no interval.
fn kept_second(keys: &[&[u8]]) -> AppsSecond {
    kept_second_of(keys, &sample_naming(keys))
}

/// The second a loader makes of a minute whose one observation names the bundles `keys`, in
/// that order, and measured no interval, with room for those Apps alone.
fn loaded_second(keys: &[&[u8]]) -> AppsSecond {
    let second = AppsSecond::with_room_for(keys.len());
    kept_second_in(second, keys, &sample_naming(keys))
}

/// The second `AppsSecond::keep` makes of a minute whose one observation, `sample`, names the
/// bundles `keys`, in that order.
fn kept_second_of(keys: &[&[u8]], sample: &SampleRow) -> AppsSecond {
    kept_second_in(AppsSecond::with_room(), keys, sample)
}

/// The second `AppsSecond::keep` makes in `second` of a minute whose one observation,
/// `sample`, names the bundles `keys`, in that order.
fn kept_second_in(mut second: AppsSecond, keys: &[&[u8]], sample: &SampleRow) -> AppsSecond {
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

    second.keep(
        &Kept {
            run: &RUN,
            sample,
            rows: &minute.apps,
            minute: &minute,
        },
        None,
    );
    second
}

/// The sample of an observation that names the bundles `keys`, found every process, and
/// measured no interval.
fn sample_naming(keys: &[&[u8]]) -> SampleRow {
    let count = u64::try_from(keys.len()).unwrap();

    SampleRow {
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
    }
}
