macro_rules! accepts_key {
    ($parse:expr, $name:ident, $kind:literal, $bytes:expr, $canonical:expr) => {
        #[test]
        fn $name() {
            // Characterize the caller's captured-key acceptance and canonical representation.
            let source = serde_json::json!({"display_name":"App",
                "key":{"kind":$kind,"bytes_hex":$bytes}});

            let key = ($parse)(&source).unwrap();

            assert_eq!(key, crate::live_source::AppKey {
                kind: $kind.into(),
                bytes_hex: $canonical.into(),
            });
        }
    };
}

macro_rules! rejects_key {
    ($parse:expr, $name:ident, $kind:expr, $bytes:expr) => {
        #[test]
        fn $name() {
            // Characterize malformed captured keys through the caller's JSON parser.
            let source = serde_json::json!({"display_name":"App",
                "key":{"kind":$kind,"bytes_hex":$bytes}});

            let result = ($parse)(&source);

            assert!(result.is_err(), "{result:?}");
        }
    };
}

macro_rules! captured_app_key_tests {
    ($parse:expr) => {
        crate::live_source::app_key_tests::accepts_key!(
            $parse,
            captured_key_accepts_lowercase_bundle,
            "bundle",
            "ab12",
            "ab12"
        );
        crate::live_source::app_key_tests::accepts_key!(
            $parse,
            captured_key_accepts_uppercase_bundle,
            "bundle",
            "ABCD",
            "abcd"
        );
        crate::live_source::app_key_tests::accepts_key!(
            $parse,
            captured_key_accepts_lowercase_executable,
            "executable",
            "ab12",
            "ab12"
        );
        crate::live_source::app_key_tests::accepts_key!(
            $parse,
            captured_key_accepts_uppercase_executable,
            "executable",
            "ABCD",
            "abcd"
        );
        crate::live_source::app_key_tests::accepts_key!(
            $parse,
            captured_key_accepts_empty_bytes,
            "bundle",
            "",
            ""
        );
        crate::live_source::app_key_tests::accepts_key!(
            $parse,
            captured_key_accepts_exact_bound,
            "bundle",
            "AB".repeat(4095),
            "ab".repeat(4095)
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_invalid_kind,
            "BUNDLE",
            "ab12"
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_null_kind,
            serde_json::Value::Null,
            "ab12"
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_non_string_kind,
            1,
            "ab12"
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_null_bytes,
            "bundle",
            serde_json::Value::Null
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_non_string_bytes,
            "bundle",
            1
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_odd_bytes,
            "executable",
            "abc"
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_non_hex_bytes,
            "bundle",
            "0g"
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_non_ascii_bytes,
            "executable",
            "é"
        );
        crate::live_source::app_key_tests::rejects_key!(
            $parse,
            captured_key_rejects_overbound_bytes,
            "bundle",
            "ab".repeat(4096)
        );
    };
}

pub(crate) use accepts_key;
pub(crate) use captured_app_key_tests;
pub(crate) use rejects_key;
