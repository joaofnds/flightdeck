use collector::disks::DISKS_MAX;
use persistence::{Error, Result};
use serde_json::Value;

use super::PointMerge;

pub(super) fn point(merge: PointMerge<'_>) -> Result<()> {
    let PointMerge { live, committed } = merge;
    if live.is_null() {
        return Ok(());
    }

    if committed.is_null() {
        return Ok(());
    }

    let devices = live["devices"]
        .as_array_mut()
        .ok_or(Error::Bound("live storage devices"))?;
    let recorded = committed["devices"]
        .as_array()
        .ok_or(Error::Bound("committed storage devices"))?;
    if devices.len() > DISKS_MAX {
        return Err(Error::Bound("live storage device count"));
    }

    if recorded.len() > DISKS_MAX {
        return Err(Error::Bound("committed storage device count"));
    }

    let mut same_inventory = devices.len() == recorded.len();
    for device in devices {
        let identity = point_identity(device)?;
        let mut matched = false;
        for saved in recorded {
            if point_identity(saved)? == identity {
                point_fields(PointMerge {
                    live: device,
                    committed: saved,
                })?;
                matched = true;
                break;
            }
        }
        if !matched {
            same_inventory = false;
        }
    }
    if same_inventory {
        if live["state"] == "complete" {
            if committed["state"] == "complete" {
                point_fields(PointMerge { live, committed })?;
            }
        }
    }

    Ok(())
}

fn point_identity(device: &Value) -> Result<(u64, u64)> {
    let driver = device["driver_id"]
        .as_u64()
        .ok_or(Error::Bound("storage driver identity"))?;
    let medium = device["media_id"]
        .as_u64()
        .ok_or(Error::Bound("storage media identity"))?;
    Ok((driver, medium))
}

fn point_fields(merge: PointMerge<'_>) -> Result<()> {
    let PointMerge { live, committed } = merge;
    for name in [
        "read_bps",
        "write_bps",
        "read_ops_per_second",
        "write_ops_per_second",
        "read_errors_per_second",
        "write_errors_per_second",
        "read_mean_ms",
        "write_mean_ms",
    ] {
        let value = live
            .get_mut(name)
            .ok_or(Error::Bound("storage rate field"))?;
        if value.is_null() {
            if let Some(recorded) = committed.get(name) {
                *value = recorded.clone();
            }
        }
    }

    Ok(())
}
