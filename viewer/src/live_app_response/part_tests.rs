use super::tests::fixture;
use super::{Detail, Error, Page, Sample, Value};

#[test]
fn gives_each_app_the_detail_its_sample_gives() {
    // Goal: a hover shows an App's detail exactly as a read of that second shows it.
    // Method: take the part of every App of a two-App sample and compare its detail with the
    // sample's own.
    let sample = fixture::sample(fixture::Scenario::Bytes);
    assert_eq!(sample.apps().len(), 2);

    for (index, app) in sample.apps().iter().enumerate() {
        let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();

        let page = Page {
            offset: 0,
            limit: 512,
        };
        assert_eq!(part.detail(page).unwrap(), detail(&sample, index, page));
    }
}

#[test]
fn pages_an_apps_processes_as_its_sample_does() {
    // Goal: every page of a large App's processes reads the same from its part.
    // Method: page a 513-process App from its first row, its last row and its exact end.
    let sample = fixture::populated(513, b"small").unwrap();
    let app = &sample.apps()[0];
    let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();

    for offset in [0, 512, 513] {
        let page = Page { offset, limit: 512 };
        assert_eq!(part.detail(page).unwrap(), detail(&sample, 0, page));
    }
}

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

#[test]
fn pages_its_seconds_apps_as_its_sample_does() {
    // Goal: a hover pages the Apps of its second as a read of that second does, without
    // keeping the Apps it lists.
    // Method: page the Apps of a two-App sample from its first row, its last row and its exact
    // end, through an App's part and through the sample.
    let sample = fixture::sample(fixture::Scenario::Bytes);
    let app = &sample.apps()[0];
    let part = sample.app_part(app.key.kind, &app.key.bytes).unwrap();

    for offset in [0, 1, 2] {
        let page = Page { offset, limit: 1 };
        let mut expected = sample.apps_page(page).unwrap();
        expected.as_object_mut().unwrap().remove("apps").unwrap();
        assert_eq!(part.apps_paging(page).unwrap(), expected);
    }
}

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

fn detail(sample: &Sample, index: usize, page: Page) -> Value {
    sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: u32::try_from(index).unwrap(),
            page,
        })
        .unwrap()
}
