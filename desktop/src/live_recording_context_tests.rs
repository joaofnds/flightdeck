use super::*;
use serde_json::json;

fn selection(count: u64) -> Value {
    assert!(count <= 4097);
    let alternatives: Vec<_> = (0..count)
        .map(|publication| {
            json!({
        "run": "00000000000000000000000000000001",
        "boot": "00000000000000000000000000000002",
        "publication": publication})
        })
        .collect();

    json!({"second": 1000, "selected": alternatives.last(), "alternatives": alternatives})
}

#[test]
fn paging_reaches_the_sixty_fifth_alternative() {
    // Goal: observation 65 stays reachable beyond the original silent 64-item cutoff.
    // Method: inspect the next page and its exact selected identity.
    let input = selection(65);

    let page = AlternativePage::read(
        RecordingContext {
            catalog: &json!({"path":"/recording"}),
            selection: &input,
        },
        1,
    )
    .unwrap();

    assert_eq!((page.index, page.count, page.total), (1, 2, 65));
    assert_eq!(page.choices.len(), 1);
    assert_eq!(page.choices[0].identity, input["selected"]);
    assert!(page.choices[0].selected);
    assert_eq!((page.previous, page.next), (Some(0), None));
}

#[test]
fn full_bound_reaches_the_last_observation_without_truncation() {
    // Goal: all 4096 backend alternatives fit in exactly 64 reachable pages.
    // Method: request the highest page with an out-of-range UI page index.
    let input = selection(4096);

    let page = AlternativePage::read(
        RecordingContext {
            catalog: &json!({"path":"/recording"}),
            selection: &input,
        },
        u16::MAX,
    )
    .unwrap();

    assert_eq!((page.index, page.count, page.total), (63, 64, 4096));
    assert_eq!(page.choices.len(), 64);
    assert_eq!(page.choices[63].identity, input["selected"]);
    assert!(page.next.is_none());
}

#[test]
fn first_page_disables_previous_and_keeps_next() {
    // Goal: navigation exposes only valid neighboring pages.
    // Method: read the first page of a two-page selection.
    let page = AlternativePage::read(
        RecordingContext {
            catalog: &json!({"path":"/recording"}),
            selection: &selection(65),
        },
        0,
    )
    .unwrap();

    assert_eq!((page.previous, page.next), (None, Some(1)));
    assert_eq!(page.choices.len(), 64);
}

#[test]
fn empty_selection_has_no_chosen_observation_or_page_navigation() {
    // Goal: an actual gap stays unselected instead of choosing a default sibling.
    // Method: read a null selection and explicitly empty alternatives.
    let page = AlternativePage::read(
        RecordingContext {
            catalog: &json!({"path":"/recording"}),
            selection: &selection(0),
        },
        42,
    )
    .unwrap();

    assert_eq!((page.index, page.count, page.total), (0, 0, 0));
    assert!(page.choices.is_empty());
    assert_eq!((page.previous, page.next), (None, None));
}

#[test]
fn rejects_more_than_the_backend_alternative_bound() {
    // Goal: excessive source results fail honestly rather than dropping observations.
    // Method: provide one more alternative than the explicit backend bound.
    assert!(
        AlternativePage::read(
            RecordingContext {
                catalog: &json!({"path":"/recording"}),
                selection: &selection(4097),
            },
            0
        )
        .is_err()
    );
}

#[test]
fn reused_publications_keep_distinct_run_and_boot_labels() {
    // Goal: publication reuse cannot make distinct observations indistinguishable.
    // Method: use the same publication across runs and boots, then choose its exact value.
    let mut input = selection(3);
    input["alternatives"][1]["publication"] = json!(0);
    input["alternatives"][1]["run"] = json!("00000000000000000000000000000003");
    input["alternatives"][2]["publication"] = json!(0);
    input["alternatives"][2]["boot"] = json!("00000000000000000000000000000004");
    input["selected"] = input["alternatives"][1].clone();
    let catalog = json!({"path":"/recording"});

    let page = AlternativePage::read(
        RecordingContext {
            catalog: &catalog,
            selection: &input,
        },
        0,
    )
    .unwrap();

    assert_ne!(page.choices[0].label, page.choices[1].label);
    assert_ne!(page.choices[0].label, page.choices[2].label);
    assert!(page.choices[1].selected);
    assert!(!page.choices[0].selected);
    assert_eq!(
        page.token.choose(&AlternativeChoice {
            context: RecordingContext {
                catalog: &catalog,
                selection: &input,
            },
            identity: &page.choices[1].identity
        }),
        Some(input["selected"].clone())
    );
}

#[test]
fn old_controls_cannot_choose_from_a_replaced_recording() {
    // Goal: queued UI actions cannot switch a sibling after another recording is opened.
    // Method: reuse the same seconds and identities with a different active source path.
    let input = selection(2);
    let page = AlternativePage::read(
        RecordingContext {
            catalog: &json!({"path":"/original"}),
            selection: &input,
        },
        0,
    )
    .unwrap();

    assert!(
        page.token
            .choose(&AlternativeChoice {
                context: RecordingContext {
                    catalog: &json!({"path":"/replacement"}),
                    selection: &input,
                },
                identity: &page.choices[0].identity
            })
            .is_none()
    );
}

#[test]
fn old_controls_cannot_replace_a_newly_chosen_sibling() {
    // Goal: a queued choice cannot overwrite a newer exact selection.
    // Method: change the selected identity without changing the wall second.
    let mut input = selection(2);
    let catalog = json!({"path":"/recording"});
    let page = AlternativePage::read(
        RecordingContext {
            catalog: &catalog,
            selection: &input,
        },
        0,
    )
    .unwrap();
    input["selected"] = input["alternatives"][0].clone();

    assert!(
        page.token
            .choose(&AlternativeChoice {
                context: RecordingContext {
                    catalog: &catalog,
                    selection: &input,
                },
                identity: &page.choices[1].identity
            })
            .is_none()
    );
}

#[test]
fn old_controls_cannot_choose_after_the_requested_second_changes() {
    // Goal: an old control cannot apply an exact identity to a different second.
    // Method: retain alternatives while changing the accepted selection's wall second.
    let mut input = selection(2);
    let catalog = json!({"path":"/recording"});
    let page = AlternativePage::read(
        RecordingContext {
            catalog: &catalog,
            selection: &input,
        },
        0,
    )
    .unwrap();
    input["second"] = json!(1001);

    assert!(
        page.token
            .choose(&AlternativeChoice {
                context: RecordingContext {
                    catalog: &catalog,
                    selection: &input,
                },
                identity: &page.choices[0].identity
            })
            .is_none()
    );
}

#[test]
fn source_context_keeps_path_extent_and_viewer_timezone_explicit() {
    // Goal: recording identity, known extent and selected-time timezone stay visible.
    // Method: format literal source metadata and a representable selected timestamp.
    let catalog = json!({"path":"/saved.recording", "first_second":101,
        "last_second":1000, "windows":2});

    let lines = source(RecordingContext {
        catalog: &catalog,
        selection: &json!({"second":1000}),
    });

    assert_eq!(lines[0], "Recording: /saved.recording");
    assert_eq!(
        lines[1],
        "Known extent: 101…1000 Unix seconds · 2 sealed windows"
    );
    assert!(lines[2].starts_with("Viewer timezone at selected second: "));
    assert!(!lines[2].ends_with("Unavailable"));
}

#[test]
fn unavailable_source_context_never_invents_wall_time_bounds() {
    // Goal: missing source metadata remains visibly unavailable.
    // Method: format absent bounds and an unrepresentable selected time.
    let lines = source(RecordingContext {
        catalog: &json!({}),
        selection: &json!({"second":u64::MAX}),
    });

    assert_eq!(lines[0], "Recording path unavailable");
    assert!(lines[1].contains("no recorded wall-time bounds"));
    assert_eq!(lines[2], "Viewer timezone at selected second: Unavailable");
}

#[test]
fn bounded_messages_keep_utf8_and_mark_truncation() {
    // Goal: error/path displays cannot allocate unbounded labels or split UTF-8.
    // Method: format text crossing a multibyte character boundary.
    assert_eq!(bounded("é🙂abcdef", 3), "é🙂a…");
}
