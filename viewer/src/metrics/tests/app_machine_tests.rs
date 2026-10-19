use super::*;

fn two_cpus() -> Fixture {
    let mut fixture = Fixture::new();
    let columns = [
        "publication",
        "cpu",
        "user_ticks",
        "system_ticks",
        "nice_ticks",
        "idle_ticks",
    ];
    *fixture.row("cpus", 1) = table(
        &columns,
        &[
            &["1", "0", "0", "0", "0", "0"],
            &["1", "1", "0", "0", "0", "0"],
        ],
    );
    *fixture.row("cpus", 2) = table(
        &columns,
        &[
            &["2", "1", "100", "0", "0", "100"],
            &["2", "0", "100", "0", "0", "0"],
        ],
    );

    fixture
}

#[test]
fn machine_cpu_sums_recorded_core_rates_with_unequal_tick_denominators() {
    // Sum a fully busy core and a half-busy core despite their different tick denominators.
    let fixture = two_cpus();

    let value = fixture.point();

    assert_eq!(value["cpu_percent_one_core"], 150.0);
    assert_eq!(value["cpu_percent"], 200.0 * 100.0 / 300.0);
    assert_ne!(
        value["cpu_percent_one_core"].as_f64(),
        value["cpu_percent"].as_f64().map(|percent| percent * 2.0)
    );
}

#[test]
fn machine_cpu_withholds_the_total_when_one_recorded_core_resets() {
    // A reset on either core makes the independent whole-machine reference unavailable.
    let mut fixture = two_cpus();
    set(fixture.row("cpus", 1), 0, "user_ticks", Some("101"));

    let value = fixture.point();

    assert_eq!(value["cpu_percent_one_core"], Value::Null);
}

#[test]
fn machine_cpu_withholds_the_total_when_one_recorded_core_is_missing() {
    // A changed CPU domain cannot become a subtotal disguised as the full machine.
    let mut fixture = two_cpus();
    fixture.row("cpus", 2).rows.pop().unwrap();
    set(fixture.row("cpus", 2), 0, "cpu", Some("0"));

    let value = fixture.point();

    assert_eq!(value["cpu_percent_one_core"], Value::Null);
}

#[test]
fn machine_cpu_requires_a_compatible_recorded_predecessor() {
    // A publication gap with complete counters still cannot produce a measured CPU rate.
    let mut fixture = two_cpus();
    fixture.previous.run_tick = 0;

    let value = fixture.point();

    assert_eq!(value["cpu_percent_one_core"], Value::Null);
}
