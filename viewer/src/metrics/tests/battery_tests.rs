use super::*;

#[test]
fn projects_fresh_battery_charge_health_and_signed_power() {
    // Goal: one source snapshot preserves charge units, above-design health and discharge sign.
    // Method: project literal normalized charge and independent full/design/electrical pairs.
    let fixture = battery_fixture();

    let point = fixture.point();

    assert_eq!(
        point["battery"],
        json!({"source_continuous_ns": 1_000_000_000_u64, "age_seconds": 2.0,
            "charge_percent": 55.0, "health_percent": 101.0, "cycle_count": 0,
            "charging": false, "external_power": false, "power_watts": -24.0,
            "adapter_watts": 94})
    );
}

#[test]
fn leaves_battery_unavailable_for_a_source_without_capture() {
    // Goal: an older source cannot manufacture battery readings from another metric.
    // Method: use the unextended literal tick source through the common scalar projection.
    let fixture = Fixture::new();

    assert!(fixture.point()["battery"].is_null());
}

macro_rules! freshness_case {
    ($name:ident, $now:expr, $available:expr) => {
        #[test]
        fn $name() {
            // Goal: battery age follows the same continuous-clock policy for every consumer.
            // Method: vary observation time while retaining one literal source acquisition.
            let mut fixture = battery_fixture();
            fixture.current.continuous = $now;

            let point = fixture.point();

            assert_eq!(point["battery"].is_object(), $available);
        }
    };
}

freshness_case!(
    retains_battery_at_the_sixty_second_age_limit,
    Some(61_000_000_000),
    true
);
freshness_case!(
    expires_battery_one_nanosecond_past_the_limit,
    Some(61_000_000_001),
    false
);
freshness_case!(
    rejects_battery_from_a_future_acquisition,
    Some(999_999_999),
    false
);
freshness_case!(
    leaves_battery_unavailable_without_a_continuous_clock,
    None,
    false
);

#[test]
fn retains_charge_and_cycles_when_battery_electrical_fields_are_missing() {
    // Goal: unavailable battery power cannot erase independently measured charge or zero cycles.
    // Method: remove only voltage from the raw source before the common projection.
    let mut fixture = battery_fixture();
    set(fixture.row("ticks", 2), 0, "battery_voltage_mv", None);

    let point = fixture.point();

    assert!(point["battery"]["power_watts"].is_null());
    assert_eq!(point["battery"]["charge_percent"], 55.0);
    assert_eq!(point["battery"]["cycle_count"], 0);
}

#[test]
fn preserves_empty_charge_and_zero_battery_flow() {
    // Goal: measured empty charge and no current remain zero rather than unavailable.
    // Method: set both source values to literal zero without changing their denominators.
    let mut fixture = battery_fixture();
    set(
        fixture.row("ticks", 2),
        0,
        "battery_charge_current",
        Some("0"),
    );
    set(fixture.row("ticks", 2), 0, "battery_current_ma", Some("0"));

    let point = fixture.point();

    assert_eq!(point["battery"]["charge_percent"], 0.0);
    assert_eq!(point["battery"]["power_watts"], 0.0);
}

#[test]
fn derives_health_only_from_full_and_design_capacity() {
    // Goal: normalized charge units cannot replace the independent health denominator.
    // Method: keep a current/max ratio of55% but remove design capacity.
    let mut fixture = battery_fixture();
    set(fixture.row("ticks", 2), 0, "battery_design_capacity", None);

    let point = fixture.point();

    assert_eq!(point["battery"]["charge_percent"], 55.0);
    assert!(point["battery"]["health_percent"].is_null());
}

#[test]
fn rejects_a_partial_battery_extension_in_the_scalar_source() {
    // Goal: incomplete capture metadata cannot silently become an older source.
    // Method: remove one named field from an otherwise complete raw table.
    let mut fixture = battery_fixture();
    fixture.row("ticks", 2).columns.pop().unwrap();
    fixture.row("ticks", 2).rows[0].pop().unwrap();

    assert!(point(&fixture.tables, &fixture.current, None, 1).is_err());
}

fn battery_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    let tick = fixture.row("ticks", 2);

    for (name, value) in [
        ("battery_version", "1"),
        ("battery_source_continuous_ns", "1000000000"),
        ("battery_charge_current", "55"),
        ("battery_charge_maximum", "100"),
        ("battery_full_charge_capacity", "101"),
        ("battery_design_capacity", "100"),
        ("battery_cycle_count", "0"),
        ("battery_charging", "false"),
        ("battery_external_power", "false"),
        ("battery_voltage_mv", "12000"),
        ("battery_current_ma", "-2000"),
        ("battery_adapter_watts", "94"),
    ] {
        tick.columns.push(name.to_owned());
        tick.rows[0].push(Some(value.to_owned()));
    }

    fixture
}
