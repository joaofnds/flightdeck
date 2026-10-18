use super::*;
use serde_json::json;

fn initialize(tables: &mut BTreeMap<&'static str, Table>) {
    tables.insert(
        "ticks",
        Table {
            columns: ["publication", "smc_read"].map(str::to_owned).to_vec(),
            rows: vec![vec![Some("7".to_owned()), Some("true".to_owned())]],
        },
    );
    tables.insert(
        "fans",
        Table {
            columns: ["publication", "fan", "rpm"].map(str::to_owned).to_vec(),
            rows: vec![
                vec![
                    Some("7".to_owned()),
                    Some("0".to_owned()),
                    Some("1000".to_owned()),
                ],
                vec![
                    Some("7".to_owned()),
                    Some("1".to_owned()),
                    Some("2000".to_owned()),
                ],
            ],
        },
    );
}

macro_rules! inconsistent_projection {
    ($name:ident, $change:expr, $reason:literal) => {
        #[test]
        fn $name() {
            // Goal: malformed source table associations never become a measured fan array.
            // Method: change one table fact before projecting the selected publication.
            let mut tables = BTreeMap::new();
            initialize(&mut tables);
            $change(&mut tables);
            let mut point = json!({"publication":7});

            let result = capture(&tables, &mut point);

            assert!(matches!(result, Err(Error::Bound($reason))), "{result:?}");
            assert!(point.get("fan_rpm").is_none());
        }
    };
}
inconsistent_projection!(
    refuses_missing_tick_rows,
    |tables: &mut BTreeMap<&str, Table>| {
        tables.get_mut("ticks").unwrap().rows.clear();
    },
    "fan tick rows"
);
inconsistent_projection!(
    refuses_duplicate_tick_rows,
    |tables: &mut BTreeMap<&str, Table>| {
        let table = tables.get_mut("ticks").unwrap();
        table.rows.push(table.rows[0].clone());
    },
    "fan tick rows"
);
inconsistent_projection!(
    refuses_another_tick_publication,
    |tables: &mut BTreeMap<&str, Table>| {
        tables.get_mut("ticks").unwrap().rows[0][0] = Some("8".to_owned());
    },
    "fan tick publication"
);
inconsistent_projection!(
    refuses_another_fan_publication,
    |tables: &mut BTreeMap<&str, Table>| {
        tables.get_mut("fans").unwrap().rows[1][0] = Some("8".to_owned());
    },
    "fan row publication"
);
inconsistent_projection!(
    refuses_a_missing_channel,
    |tables: &mut BTreeMap<&str, Table>| {
        tables.get_mut("fans").unwrap().rows[1][1] = Some("2".to_owned());
    },
    "fan channel order"
);
inconsistent_projection!(
    refuses_a_duplicate_channel,
    |tables: &mut BTreeMap<&str, Table>| {
        tables.get_mut("fans").unwrap().rows[1][1] = Some("0".to_owned());
    },
    "fan channel order"
);
inconsistent_projection!(
    refuses_more_than_ten_channels,
    |tables: &mut BTreeMap<&str, Table>| {
        tables
            .get_mut("fans")
            .unwrap()
            .rows
            .resize(11, vec![None; 3]);
    },
    "fan channels"
);
inconsistent_projection!(
    refuses_rows_from_an_unread_smc,
    |tables: &mut BTreeMap<&str, Table>| {
        tables.get_mut("ticks").unwrap().rows[0][1] = Some("false".to_owned());
    },
    "unread fan rows"
);
inconsistent_projection!(
    refuses_malformed_channel_text,
    |tables: &mut BTreeMap<&str, Table>| {
        tables.get_mut("fans").unwrap().rows[0][2] = Some("not a number".to_owned());
    },
    "number"
);

#[test]
fn keeps_a_nullable_channel_as_a_gap() {
    // Goal: missing channel gauges stay absent while other channels remain measured.
    // Method: project a nullable RPM cell beside an independent positive fan.
    let mut tables = BTreeMap::new();
    initialize(&mut tables);
    tables.get_mut("fans").unwrap().rows[0][2] = None;
    let mut point = json!({"publication":7});

    capture(&tables, &mut point).unwrap();

    assert_eq!(point["fan_rpm"], json!([null, 2000.0]));
}

macro_rules! nonfinite_projection {
    ($name:ident, $rpm:literal) => {
        #[test]
        fn $name() {
            // Goal: a parsed nonfinite fan remains a gap beside a healthy channel.
            // Method: project an invalid numeric gauge through the fan-specific boundary.
            let mut tables = BTreeMap::new();
            initialize(&mut tables);
            tables.get_mut("fans").unwrap().rows[0][2] = Some($rpm.to_owned());
            let mut point = json!({"publication":7});

            capture(&tables, &mut point).unwrap();

            assert_eq!(point["fan_rpm"], json!([null, 2000.0]));
        }
    };
}
nonfinite_projection!(keeps_nan_channel_as_a_gap, "NaN");
nonfinite_projection!(keeps_infinite_channel_as_a_gap, "inf");
nonfinite_projection!(keeps_negative_infinite_channel_as_a_gap, "-inf");
