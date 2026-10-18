use super::tests::fixture;
use super::{Detail, Error, Page, SAMPLE_BYTES_MAX, Sample, Value};

#[test]
fn pages_members_without_losing_the_selected_observation() {
    // Goal: more than one process page stays attached to one exact selected observation.
    // Method: request the first 512 members and the final member of one 513-member App.
    let sample = fixture::populated(513, b"small").unwrap();

    let first = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 512,
            },
        })
        .unwrap();
    let second = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 512,
                limit: 512,
            },
        })
        .unwrap();

    assert_eq!(first["processes"].as_array().unwrap().len(), 512);
    assert_eq!(
        first["page"],
        serde_json::json!({
            "offset": 0, "limit": 512, "total": 513, "next_offset": 512
        })
    );
    assert_eq!(second["processes"].as_array().unwrap().len(), 1);
    assert_eq!(second["processes"][0]["pid"], 513);
    assert_eq!(second["page"]["next_offset"], Value::Null);
    assert_eq!(first["selected"], second["selected"]);
}

macro_rules! invalid_page {
    ($name:ident, $offset:literal, $limit:literal, $error:literal) => {
        #[test]
        fn $name() {
            // Goal: page admission enforces its exact public bounds.
            // Method: vary one named page boundary against one populated App.
            let sample = fixture::sample(fixture::Scenario::Ordinary);

            let result = sample.apps_page(Page {
                offset: $offset,
                limit: $limit,
            });

            assert!(matches!(result, Err(Error::Bound($error))));
        }
    };
}
invalid_page!(rejects_a_zero_page_limit, 0, 0, "live App page limit");
invalid_page!(rejects_more_than_512_rows, 0, 513, "live App page limit");
invalid_page!(
    rejects_a_page_start_after_the_last_app,
    2,
    512,
    "live App page offset"
);

#[test]
fn permits_an_empty_page_at_the_exact_end() {
    // Goal: reaching the last row terminates paging without a fabricated continuation.
    // Method: request the exact end of a one-App sample.
    let sample = fixture::sample(fixture::Scenario::Ordinary);

    let page = sample
        .apps_page(Page {
            offset: 1,
            limit: 512,
        })
        .unwrap();

    assert_eq!(page["apps"], serde_json::json!([]));
    assert_eq!(page["page"]["next_offset"], Value::Null);
}

#[test]
fn charges_escaped_capacity_before_accepting_a_large_sample() {
    // Goal: owned raw bytes and display capacity both count against retained sample memory.
    // Method: retain 400 maximum-length names, each requiring four display bytes per source byte.
    let name = [255; 4095];

    let sample = fixture::populated(400, &name).unwrap();

    assert!(sample.bytes() > 8_000_000);
    assert!(sample.bytes() <= 8_388_608);
    assert_eq!(sample.processes().len(), 400);
    assert_eq!(SAMPLE_BYTES_MAX, 8_388_608);
}

#[test]
fn refuses_owned_samples_over_eight_mebibytes() {
    // Goal: large captured names cannot grow retained presentation beyond the fixed memory budget.
    // Method: exceed the budget with 420 maximum-length names.
    let name = [255; 4095];

    let result = fixture::populated(420, &name);

    assert!(matches!(result, Err(Error::Bound("live App sample bytes"))));
}

#[test]
fn refuses_large_wire_pages_but_keeps_smaller_pages_available() {
    // Goal: escaping and reversible hex cannot exceed the bridge response budget.
    // Method: a bounded sample has a large expanded wire page and a smaller valid page.
    let sample = fixture::populated(400, &[255; 4095]).unwrap();

    let large = sample.app_detail(&Detail {
        selected: &sample.selected,
        app_index: 0,
        page: Page {
            offset: 0,
            limit: 512,
        },
    });
    let small = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 128,
            },
        })
        .unwrap();

    assert!(matches!(
        large,
        Err(Error::Bound("live App response bytes"))
    ));
    assert_eq!(small["processes"].as_array().unwrap().len(), 128);
}

#[test]
fn refuses_more_than_the_bounded_process_population() {
    // Goal: response work remains bounded even if an upstream aggregate is malformed.
    // Method: supply one more than the admitted 24,000 process intervals.
    let result = fixture::populated(24_001, b"small");

    assert!(matches!(result, Err(Error::Bound("live App member count"))));
}

#[test]
fn charges_all_retained_string_and_vector_capacities() {
    // Goal: cache admission sees typed storage in addition to captured text length.
    // Method: compare the public byte charge with independently enumerated retained allocations.
    let sample = fixture::sample(fixture::Scenario::Ordinary);

    let actual = sample.bytes();

    assert_eq!(
        actual,
        size_of::<Sample>()
            + sample.apps.capacity() * size_of::<super::App>()
            + sample.processes.capacity() * size_of::<super::Process>()
            + 6
            + 24
            + 20
            + 4
            + 16
            + 6
            + 24
    );
}

#[test]
fn keeps_disk_layouts_inside_the_unchanged_sample_and_response_budgets() {
    // Goal: layout growth remains explicit without widening either fixed eight-MiB budget.
    // Method: admit the full short-name population and serialize its first bounded member page.
    let sample = fixture::populated(24_000, b"small").unwrap();

    let page = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 512,
            },
        })
        .unwrap();

    println!(
        "{}",
        serde_json::json!({
            "aggregate_app_bytes":size_of::<crate::apps::AppTotals>(),
            "aggregate_process_bytes":size_of::<crate::apps::ProcessTotals<'_>>(),
            "owned_app_bytes":size_of::<super::App>(),
            "owned_process_bytes":size_of::<super::Process>(),
            "sample_bytes":sample.bytes(), "sample_budget":SAMPLE_BYTES_MAX,
            "response_budget":super::RESPONSE_BYTES_MAX
        })
    );

    assert_eq!(SAMPLE_BYTES_MAX, 8_388_608);
    assert_eq!(super::RESPONSE_BYTES_MAX, 8_388_608);
    assert!(sample.bytes() <= SAMPLE_BYTES_MAX);
    assert_eq!(page["page"]["total"], 24_000);
    assert_eq!(page["processes"].as_array().unwrap().len(), 512);
}

#[test]
fn preserves_generated_captured_names_through_the_wire_encoding() {
    // Goal: reversible bytes and escaped displays cannot collapse distinct generated names.
    // Method: round-trip 128 deterministic valid basenames containing arbitrary nonzero bytes.
    let mut seed = 0x3e46_1224_u64;
    for case in 0..128_u32 {
        let mut bytes = Vec::with_capacity(64);
        for _index in 0..64 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let mut byte = u8::try_from((seed >> 32) % 255 + 1).unwrap();
            if byte == b'/' {
                byte = b'0';
            }
            bytes.push(byte);
        }
        let sample = fixture::populated(1, &bytes).unwrap();

        let result = sample
            .app_detail(&Detail {
                selected: &sample.selected,
                app_index: 0,
                page: Page {
                    offset: 0,
                    limit: 512,
                },
            })
            .unwrap();
        let encoded = result["processes"][0]["executable_name"]["bytes_hex"]
            .as_str()
            .unwrap();

        assert_eq!(decode_hex(encoded), bytes, "generated case {case}");
    }
}

fn decode_hex(encoded: &str) -> Vec<u8> {
    encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

#[test]
fn refuses_a_captured_name_one_byte_above_the_source_limit() {
    // Goal: display/hex expansion begins only after the source byte limit is checked.
    // Method: pass 4096 bytes against the supported 4095-byte captured field.
    let result = fixture::populated(1, &[255; 4096]);

    assert!(matches!(
        result,
        Err(Error::Bound("live App captured bytes"))
    ));
}

#[test]
fn rejects_an_app_index_past_the_selected_app_list() {
    // Goal: detail cannot resolve a nonexistent sample-relative App index.
    // Method: request index one from a sample containing only index zero.
    let sample = fixture::sample(fixture::Scenario::Ordinary);

    let result = sample.app_detail(&Detail {
        selected: &sample.selected,
        app_index: 1,
        page: Page {
            offset: 0,
            limit: 512,
        },
    });

    assert!(matches!(result, Err(Error::Bound("live App index"))));
}
