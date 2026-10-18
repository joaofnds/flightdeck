use serde_json::{Value, json};

use super::Target;

const RUN: &str = "00000000000000010000000000000002";
const BOOT: &str = "00000000000000030000000000000004";

macro_rules! identity_case {
    ($name:ident, $observation:expr, $matches:expr) => {
        #[test]
        fn $name() {
            // Check the full observation identity across the recording's accepted wire forms.
            let target = Target::exact(1000, &json!({"run":RUN,"boot":BOOT,"publication":42}))
                .unwrap();
            let observation: Value = $observation;

            assert_eq!(target.matches(&observation), $matches);
        }
    };
}

identity_case!(
    matches_canonical_identity,
    json!({"run":RUN,"boot":BOOT,"publication":42}),
    true
);
identity_case!(
    matches_string_publication,
    json!({"run":RUN,"boot":BOOT,"publication":"42"}),
    true
);
identity_case!(
    matches_mixed_identity_words,
    json!({"run":[1,"2"],"boot":["3",4],"publication":"42"}),
    true
);
identity_case!(
    matches_nested_stamp,
    json!({"stamp":{"run":RUN,"boot":BOOT,"publication":42}}),
    true
);
identity_case!(
    rejects_another_publication,
    json!({"run":RUN,"boot":BOOT,"publication":43}),
    false
);
identity_case!(
    rejects_reused_publication_from_another_run,
    json!({"run":[2,1],"boot":BOOT,"publication":42}),
    false
);
identity_case!(
    rejects_reused_publication_from_another_boot,
    json!({"run":RUN,"boot":[4,3],"publication":42}),
    false
);
identity_case!(
    rejects_missing_publication,
    json!({"run":RUN,"boot":BOOT}),
    false
);
identity_case!(
    rejects_overflowed_publication,
    json!({"run":RUN,"boot":BOOT,"publication":"18446744073709551616"}),
    false
);
identity_case!(
    rejects_negative_publication,
    json!({"run":RUN,"boot":BOOT,"publication":-1}),
    false
);
identity_case!(
    rejects_fractional_publication,
    json!({"run":RUN,"boot":BOOT,"publication":42.5}),
    false
);
identity_case!(
    rejects_invalid_run,
    json!({"run":"not a run","boot":BOOT,"publication":42}),
    false
);
identity_case!(
    rejects_invalid_boot,
    json!({"run":RUN,"boot":[3],"publication":42}),
    false
);
identity_case!(
    rejects_invalid_nested_stamp_despite_valid_outer_identity,
    json!({"run":RUN,"boot":BOOT,"publication":42,"stamp":null}),
    false
);

#[test]
fn matches_hexadecimal_case_without_weakening_identity() {
    // Hexadecimal normalization is independent of publication representation.
    let target = Target::exact(
        1000,
        &json!({"run":"abcdef0123456789abcdef0123456789","boot":BOOT,"publication":42}),
    )
    .unwrap();
    let observation = json!({"run":"ABCDEF0123456789ABCDEF0123456789", "boot":BOOT,
        "publication":"42"});

    assert!(target.matches(&observation));
}
