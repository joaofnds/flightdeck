use super::*;
use crate::metric_view::View;
use crate::recording::{FanRow, MachineMinute, TickRow};
use serde_json::json;

const PUBLICATION: u64 = 7;

fn minute() -> MachineMinute {
    MachineMinute {
        ticks: vec![TickRow::fixture(PUBLICATION)],
        fans: vec![fan(0, 1000.0), fan(1, 2000.0)],
        ..MachineMinute::default()
    }
}

fn fan(fan: u32, rpm: f32) -> FanRow {
    FanRow {
        publication: PUBLICATION,
        fan,
        rpm,
    }
}

fn project(minute: &MachineMinute, point: &mut Value) -> Result<()> {
    let reading = crate::store::reading_at(minute, PUBLICATION).unwrap();
    let ticks = View(crate::live_raw::View::machine(&reading, "ticks")?);
    let fans = View(crate::live_raw::View::machine(&reading, "fans")?);
    let value = capture_view(&ticks, Some(&fans), point["publication"].as_u64().unwrap())?;
    point["fan_rpm"] = value;
    Ok(())
}

macro_rules! inconsistent_projection {
    ($name:ident, $change:expr, $reason:literal) => {
        #[test]
        fn $name() {
            // Goal: malformed source rows never become a measured fan array.
            // Method: change one fact before projecting the selected publication.
            let mut minute = minute();
            $change(&mut minute);
            let mut point = json!({"publication":PUBLICATION});

            let result = project(&minute, &mut point);

            assert!(matches!(result, Err(Error::Bound($reason))), "{result:?}");
            assert!(point.get("fan_rpm").is_none());
        }
    };
}
inconsistent_projection!(
    refuses_a_missing_channel,
    |minute: &mut MachineMinute| minute.fans[1].fan = 2,
    "fan channel order"
);
inconsistent_projection!(
    refuses_a_duplicate_channel,
    |minute: &mut MachineMinute| minute.fans[1].fan = 0,
    "fan channel order"
);
inconsistent_projection!(
    refuses_more_than_ten_channels,
    |minute: &mut MachineMinute| minute.fans.resize(11, fan(0, 0.0)),
    "fan channels"
);
inconsistent_projection!(
    refuses_rows_from_an_unread_smc,
    |minute: &mut MachineMinute| minute.ticks[0].smc_read = false,
    "unread fan rows"
);

#[test]
fn refuses_another_tick_publication() {
    // Goal: a tick cannot answer for a publication it is not.
    // Method: project the tick for the publication after it.
    let mut point = json!({"publication":PUBLICATION + 1});

    let result = project(&minute(), &mut point);

    assert!(
        matches!(result, Err(Error::Bound("fan tick publication"))),
        "{result:?}"
    );
}

macro_rules! nonfinite_projection {
    ($name:ident, $rpm:expr) => {
        #[test]
        fn $name() {
            // Goal: a nonfinite fan remains a gap beside a healthy channel.
            // Method: project an invalid numeric gauge through the fan-specific boundary.
            let mut minute = minute();
            minute.fans[0].rpm = $rpm;
            let mut point = json!({"publication":PUBLICATION});

            project(&minute, &mut point).unwrap();

            assert_eq!(point["fan_rpm"], json!([null, 2000.0]));
        }
    };
}
nonfinite_projection!(keeps_nan_channel_as_a_gap, f32::NAN);
nonfinite_projection!(keeps_infinite_channel_as_a_gap, f32::INFINITY);
nonfinite_projection!(keeps_negative_infinite_channel_as_a_gap, f32::NEG_INFINITY);
