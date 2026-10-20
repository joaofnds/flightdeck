use super::tests::fixture;
use super::{Detail, Error, Page, Process, Sample, Value};

macro_rules! app_detail_case {
    ($name:ident, $index:expr) => {
        #[test]
        fn $name() {
            // Goal: a hover shows an App's detail exactly as a read of that second shows it.
            // Method: take the part of one App of a two-App sample and compare its detail with
            // the sample's own.
            let sample = fixture::sample(fixture::Scenario::Bytes);
            assert_eq!(sample.apps().len(), 2);
            let app = &sample.apps()[$index];

            let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();

            let page = Page {
                offset: 0,
                limit: 512,
            };
            assert_eq!(part.detail(page).unwrap(), detail(&sample, $index, page));
        }
    };
}
app_detail_case!(gives_the_first_app_the_detail_its_sample_gives, 0);
app_detail_case!(gives_the_second_app_the_detail_its_sample_gives, 1);

macro_rules! process_page_case {
    ($name:ident, $offset:expr) => {
        #[test]
        fn $name() {
            // Goal: every page of a large App's processes reads the same from its part.
            // Method: page a 513-process App at the stated offset, through its part and
            // through its sample.
            let sample = fixture::populated(513, b"small").unwrap();
            let app = &sample.apps()[0];
            let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();
            let page = Page {
                offset: $offset,
                limit: 512,
            };

            let paged = part.detail(page).unwrap();

            assert_eq!(paged, detail(&sample, 0, page));
        }
    };
}
process_page_case!(pages_an_apps_processes_from_their_first_row, 0);
process_page_case!(pages_an_apps_processes_from_their_last_row, 512);
process_page_case!(pages_an_apps_processes_at_their_exact_end, 513);

#[test]
fn gives_no_detail_for_an_app_absent_from_its_second() {
    // Goal: a second without the selected App shows no detail, as a read of it does.
    // Method: take the part of a key no App of the sample has.
    let sample = fixture::sample(fixture::Scenario::Ordinary);

    let part = sample
        .app_part(super::KeyKind::Executable, b"absent")
        .unwrap();

    let page = Page {
        offset: 0,
        limit: 512,
    };
    assert_eq!(part.detail(page).unwrap(), Value::Null);
    assert_eq!(part.selected(), &sample.selected);
}

#[test]
fn refuses_a_page_its_sample_refuses() {
    // Goal: a part admits only the pages its sample admits.
    // Method: page past the end of an App's processes.
    let sample = fixture::sample(fixture::Scenario::Ordinary);
    let app = &sample.apps()[0];
    let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();

    let result = part.detail(Page {
        offset: 3,
        limit: 512,
    });

    assert!(matches!(result, Err(Error::Bound("live App page offset"))));
}

#[test]
fn knows_the_app_it_belongs_to() {
    // Goal: a hover never shows a part of another App as the selected App's.
    // Method: ask a part about its own key, the same bytes of the other kind, and other bytes.
    let sample = fixture::sample(fixture::Scenario::Ordinary);
    let app = &sample.apps()[0];

    let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();

    assert!(part.is_of(app.key.kind, &app.key.bytes));
    assert!(!part.is_of(super::KeyKind::Executable, &app.key.bytes));
    assert!(!part.is_of(app.key.kind, b"other"));
}

macro_rules! apps_page_case {
    ($name:ident, $offset:expr) => {
        #[test]
        fn $name() {
            // Goal: a hover pages the Apps of its second as a read of that second does, without
            // keeping the Apps it lists.
            // Method: page the Apps of a two-App sample at the stated offset, through an App's
            // part and through the sample.
            let sample = fixture::sample(fixture::Scenario::Bytes);
            let app = &sample.apps()[0];
            let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();
            let page = Page {
                offset: $offset,
                limit: 1,
            };

            let paged = part.apps_paging(page).unwrap();

            let mut expected = sample.apps_page(page).unwrap();
            expected.as_object_mut().unwrap().remove("apps").unwrap();
            assert_eq!(paged, expected);
        }
    };
}
apps_page_case!(pages_its_seconds_apps_from_their_first_row, 0);
apps_page_case!(pages_its_seconds_apps_from_their_last_row, 1);
apps_page_case!(pages_its_seconds_apps_at_their_exact_end, 2);

#[test]
fn pages_the_apps_of_a_second_without_its_app() {
    // Goal: a hover still pages its second's Apps where the selected App did not run.
    // Method: page the Apps through the part of a key no App of the sample has.
    let sample = fixture::sample(fixture::Scenario::Bytes);
    let part = sample
        .app_part(super::KeyKind::Executable, b"absent")
        .unwrap();

    let page = Page {
        offset: 0,
        limit: 512,
    };
    let mut expected = sample.apps_page(page).unwrap();
    expected.as_object_mut().unwrap().remove("apps").unwrap();
    assert_eq!(part.apps_paging(page).unwrap(), expected);
}

#[test]
fn refuses_an_apps_page_its_sample_refuses() {
    // Goal: a part pages only as far as its sample's Apps go.
    // Method: page the Apps of a two-App sample past their end.
    let sample = fixture::sample(fixture::Scenario::Bytes);
    let app = &sample.apps()[0];
    let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();

    let result = part.apps_paging(Page {
        offset: 3,
        limit: 512,
    });

    assert!(matches!(result, Err(Error::Bound("live App page offset"))));
}

#[test]
fn counts_every_process_of_its_app_in_its_bytes() {
    // Goal: the bound on a window's parts sees each process a part keeps.
    // Method: take the part of a one-process App and of a 513-process App.
    let one = fixture::populated(1, b"small").unwrap();
    let many = fixture::populated(513, b"small").unwrap();
    let app = &one.apps()[0];

    let small = one.app_part(app.key.kind, &app.key.bytes).unwrap();
    let large = many.app_part(app.key.kind, &app.key.bytes).unwrap();

    assert!(large.bytes() >= small.bytes() + 512 * size_of::<Process>());
}

fn detail(sample: &Sample, index: usize, page: Page) -> Value {
    sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: u32::try_from(index).unwrap(),
            page,
        })
        .unwrap()
}
