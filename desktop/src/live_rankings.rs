use std::cmp::Ordering;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::live_source::{AppKey, identity_signature, identity_validate};

#[path = "live_rankings_view.rs"]
mod view;

pub const RANKS_MAX: usize = 10;
const APPS_MAX: u32 = 24_000;
const PAGE_ROWS: u32 = 512;
const PAGES_MAX: u32 = APPS_MAX.div_ceil(PAGE_ROWS);
const PROGRAM_BYTES_MAX: usize = 4095;
const _: () = {
    assert!(PAGES_MAX == 47);
    assert!(RANKS_MAX <= 10);
    assert!(APPS_MAX == 24_000);
    assert!(viewer::live_app_response::APPS_MAX == 24_000);
    assert!(PAGE_ROWS == viewer::live_app_response::PAGE_ROWS_MAX);
};

#[derive(Debug)]
pub enum Rankings {
    Ready(TopApps),
    Unavailable(String),
}

#[derive(Debug)]
pub struct TopApps {
    pub selected: Value,
    pub app_count: u32,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    pub metrics: [Ranking; 3],
}

#[derive(Debug)]
pub struct Ranking {
    pub metric: Metric,
    pub entries: Vec<Entry>,
    pub unavailable: u32,
    pub overflowed: u32,
    pub partial: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum Metric {
    Cpu,
    Resident,
    Power,
}

#[derive(Debug)]
pub struct Entry {
    pub app: Arc<App>,
    pub amount: u64,
    pub rate: Option<f64>,
    pub measured_members: u32,
    pub unavailable_members: u32,
}

#[derive(Debug)]
pub struct App {
    pub key: AppKey,
    pub name: String,
}

struct Reading {
    amount: u64,
    rate: Option<f64>,
    measured: u32,
    unavailable: u32,
}

pub fn read(selected: &Value, page: impl FnMut(u32) -> Result<Value, String>) -> Rankings {
    match read_pages(selected, page) {
        Ok(rankings) => Rankings::Ready(rankings),
        Err(error) => {
            Rankings::Unavailable(crate::live_notices::recording_context::bounded(&error, 512))
        }
    }
}

fn read_pages(
    selected: &Value,
    mut page: impl FnMut(u32) -> Result<Value, String>,
) -> Result<TopApps, String> {
    let (run, boot, publication) = identity_signature(selected)?;
    let mut result = TopApps {
        selected: json!({"run":run,"boot":boot,"publication":publication}),
        app_count: 0,
        membership_complete: false,
        ungrouped_members: 0,
        metrics: [Metric::Cpu, Metric::Resident, Metric::Power].map(Ranking::new),
    };

    for index in 0..PAGES_MAX {
        let offset = index * PAGE_ROWS;
        let source = page(offset)?;
        identity_validate(&source["selected"], selected)?;
        read_header(&source, offset, &mut result)?;
        let apps = source["apps"]
            .as_array()
            .ok_or("Missing ranking App rows")?;
        let count = u32::try_from(apps.len()).map_err(|error| error.to_string())?;
        let expected = result.app_count.saturating_sub(offset).min(PAGE_ROWS);
        if count != expected {
            return Err("Ranking App page has an inconsistent row count".into());
        }

        for app in apps {
            read_app(app, &mut result.metrics)?;
        }

        let end = offset + count;
        if end == result.app_count {
            if !source["page"]["next_offset"].is_null() {
                return Err("Final ranking page has a next offset".into());
            }

            return Ok(result);
        }

        if source["page"]["next_offset"].as_u64() != Some(u64::from(end)) {
            return Err("Ranking App page has an inconsistent next offset".into());
        }
    }

    Err("Ranking App pages exceed the source bound".into())
}

fn read_header(source: &Value, offset: u32, result: &mut TopApps) -> Result<(), String> {
    let total = count(&source["page"]["total"])?;
    if total > APPS_MAX {
        return Err("Ranking App count exceeds 24000".into());
    }

    if count(&source["app_count"])? != total {
        return Err("Ranking App count differs from its page total".into());
    }

    if count(&source["page"]["offset"])? != offset {
        return Err("Ranking App page has a different offset".into());
    }

    if count(&source["page"]["limit"])? != PAGE_ROWS {
        return Err("Ranking App page has a different limit".into());
    }

    let complete = source["membership_complete"]
        .as_bool()
        .ok_or("Missing ranking membership coverage")?;
    let ungrouped = count(&source["ungrouped_members"])?;

    if offset == 0 {
        result.app_count = total;
        result.membership_complete = complete;
        result.ungrouped_members = ungrouped;
    } else {
        if total != result.app_count {
            return Err("Ranking App count changed between pages".into());
        }

        if complete != result.membership_complete {
            return Err("Ranking membership coverage changed between pages".into());
        }

        if ungrouped != result.ungrouped_members {
            return Err("Ranking ungrouped membership changed between pages".into());
        }
    }

    Ok(())
}

fn read_app(source: &Value, metrics: &mut [Ranking; 3]) -> Result<(), String> {
    let mut app = None;
    for ranking in metrics {
        let Some(reading) = ranking.read_measure(source)? else {
            continue;
        };

        if ranking.entries.len() == RANKS_MAX {
            if reading.amount < ranking.entries[RANKS_MAX - 1].amount {
                continue;
            }
        }

        if app.is_none() {
            app = Some(Arc::new(read_app_identity(source)?));
        }

        let candidate = Entry {
            app: Arc::clone(app.as_ref().ok_or("Missing ranking App identity")?),
            amount: reading.amount,
            rate: reading.rate,
            measured_members: reading.measured,
            unavailable_members: reading.unavailable,
        };

        ranking.insert(candidate);
    }

    Ok(())
}

fn read_app_identity(source: &Value) -> Result<App, String> {
    let kind = source["key"]["kind"]
        .as_str()
        .ok_or("Missing ranking App kind")?;
    if !matches!(kind, "bundle" | "executable") {
        return Err("Invalid ranking App kind".into());
    }

    let bytes = source["key"]["bytes_hex"]
        .as_str()
        .ok_or("Missing ranking App bytes")?;
    if bytes.len() > PROGRAM_BYTES_MAX * 2 {
        return Err("Ranking App key exceeds the captured path bound".into());
    }

    if !bytes.len().is_multiple_of(2) {
        return Err("Ranking App key contains an incomplete byte".into());
    }

    if !bytes.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Ranking App key contains invalid hexadecimal bytes".into());
    }

    let name = source["display_name"]
        .as_str()
        .ok_or("Missing ranking App name")?;
    if name.len() > PROGRAM_BYTES_MAX * 4 {
        return Err("Ranking App name exceeds the escaped path bound".into());
    }

    Ok(App {
        key: AppKey {
            kind: kind.into(),
            bytes_hex: bytes.to_ascii_lowercase(),
        },
        name: name.into(),
    })
}

impl Ranking {
    fn read_measure(&mut self, source: &Value) -> Result<Option<Reading>, String> {
        let measure = &source[self.metric.measure()];
        if measure["overflowed"]
            .as_bool()
            .ok_or("Missing ranking overflow state")?
        {
            self.overflowed += 1;
            return Ok(None);
        }

        let measured = count(&measure["measured_members"])?;
        if measured == 0 {
            self.unavailable += 1;
            return Ok(None);
        }

        let rate = self.metric.rate(source)?;
        if !matches!(self.metric, Metric::Resident) {
            if rate.is_none() {
                self.unavailable += 1;
                return Ok(None);
            }
        }

        let unavailable = count(&measure["unavailable_members"])?;
        if unavailable > 0 {
            self.partial += 1;
        }

        let amount = measure["known"]
            .as_str()
            .filter(|text| text.len() <= 20)
            .ok_or("Missing exact ranking total")?
            .parse::<u64>()
            .map_err(|error| error.to_string())?;

        Ok(Some(Reading {
            amount,
            rate,
            measured,
            unavailable,
        }))
    }

    fn new(metric: Metric) -> Self {
        Self {
            metric,
            entries: Vec::with_capacity(RANKS_MAX + 1),
            unavailable: 0,
            overflowed: 0,
            partial: 0,
        }
    }

    fn insert(&mut self, candidate: Entry) {
        let index = self
            .entries
            .partition_point(|entry| entry.compare(&candidate).is_lt());
        if index < RANKS_MAX {
            self.entries.insert(index, candidate);
            if self.entries.len() > RANKS_MAX {
                assert!(self.entries.pop().is_some());
            }
        }
        assert!(self.entries.len() <= RANKS_MAX);
    }
}

impl Entry {
    fn compare(&self, other: &Self) -> Ordering {
        other
            .amount
            .cmp(&self.amount)
            .then(self.app.key.kind.cmp(&other.app.key.kind))
            .then(self.app.key.bytes_hex.cmp(&other.app.key.bytes_hex))
    }
}

impl Metric {
    pub fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Resident => "Resident memory",
            Self::Power => "Process power",
        }
    }

    fn measure(self) -> &'static str {
        match self {
            Self::Cpu => "cpu_ns",
            Self::Resident => "resident_bytes",
            Self::Power => "energy_nj",
        }
    }

    fn rate(self, source: &Value) -> Result<Option<f64>, String> {
        let key = match self {
            Self::Cpu => "cpu_percent_one_core",
            Self::Power => "energy_watts",
            Self::Resident => return Ok(None),
        };
        if source[key].is_null() {
            return Ok(None);
        }

        let value = source[key].as_f64().ok_or("Invalid ranking rate")?;
        if !value.is_finite() {
            return Err("Nonfinite ranking rate".into());
        }

        if value < 0.0 {
            return Err("Negative ranking rate".into());
        }

        Ok(Some(value))
    }
}

fn count(value: &Value) -> Result<u32, String> {
    let value = value.as_u64().ok_or("Missing ranking count")?;
    u32::try_from(value).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "live_rankings_tests.rs"]
mod tests;
