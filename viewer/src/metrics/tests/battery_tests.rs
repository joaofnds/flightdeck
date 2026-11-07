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
    battery_of(&mut fixture).voltage_mv = None;

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
    battery_of(&mut fixture).charge_current = Some(0);
    battery_of(&mut fixture).current_ma = Some(0);

    let point = fixture.point();

    assert_eq!(point["battery"]["charge_percent"], 0.0);
    assert_eq!(point["battery"]["power_watts"], 0.0);
}

#[test]
fn derives_health_only_from_full_and_design_capacity() {
    // Goal: normalized charge units cannot replace the independent health denominator.
    // Method: keep a current/max ratio of55% but remove design capacity.
    let mut fixture = battery_fixture();
    battery_of(&mut fixture).design_capacity = None;

    let point = fixture.point();

    assert_eq!(point["battery"]["charge_percent"], 55.0);
    assert!(point["battery"]["health_percent"].is_null());
}

fn battery_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.minute(2).ticks[0].battery = Some(collector::battery::BatteryReading {
        source_continuous_ns: 1_000_000_000,
        charge_current: Some(55),
        charge_maximum: Some(100),
        full_charge_capacity: Some(101),
        design_capacity: Some(100),
        cycle_count: Some(0),
        charging: Some(false),
        external_power: Some(false),
        voltage_mv: Some(12_000),
        current_ma: Some(-2_000),
        adapter_watts: Some(94),
    });

    fixture
}

fn battery_of(fixture: &mut Fixture) -> &mut collector::battery::BatteryReading {
    fixture.minute(2).ticks[0].battery.as_mut().unwrap()
}
