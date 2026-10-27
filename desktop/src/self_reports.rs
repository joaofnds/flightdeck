//! The collector's own reports of what collecting costs, read from the output of the collector
//! this app started. The collector writes a report every ten ticks, one every minute with the
//! persister's cost, and the persister writes a line for every window it seals.

use std::collections::VecDeque;

/// Fifteen minutes of ten-second reports, as long as every chart of the app spans.
pub const TICK_REPORTS_MAX: usize = 90;
pub const MINUTE_REPORTS_MAX: usize = 15;
pub const SEALS_MAX: usize = 16;
/// The collector's cost sources and its unattributed remainder.
pub const SOURCES_MAX: usize = 16;
/// The seconds between two ten-second reports, ten ticks of one second.
pub const TICK_REPORT_SECONDS: u64 = 10;
/// The seconds between two minute reports.
pub const MINUTE_REPORT_SECONDS: u64 = 60;

#[derive(Clone, Copy)]
pub enum OutputStream {
    Stdout,
    Stderr,
}
const _: () = {
    assert!(TICK_REPORTS_MAX * 10 == 900);
    assert!(MINUTE_REPORTS_MAX * 60 == 900);
    assert!(MINUTE_REPORT_SECONDS.is_multiple_of(TICK_REPORT_SECONDS));
};

/// One ten-second report: what collecting cost over its ticks, and what the last tick read.
#[derive(Clone, Debug, PartialEq)]
pub struct TickReport {
    pub tick_first: u64,
    pub tick_last: u64,
    /// The wall second at which this app read the report.
    pub second: u64,
    pub collector_cpu_percent: f32,
    pub collector_milliwatts: f32,
    pub late_average_ms: f32,
    pub late_maximum_ms: f32,
    pub slips: u64,
    /// Milliseconds of CPU each source cost per elapsed second, in the collector's order.
    pub sources: Vec<(String, f32)>,
    pub processes_observed: u64,
    pub processes_refused: u64,
    pub processes_gone: u64,
    /// Starts, execs and exits dropped from a full event list.
    pub event_drops: u64,
}

/// One minute of the recording's cost, the collector's and the persister's together.
#[derive(Clone, Debug, PartialEq)]
pub struct MinuteReport {
    pub second: u64,
    pub elapsed_ns: u64,
    pub collector_cpu_ns: u64,
    pub persister_cpu_ns: u64,
    pub combined_percent: f32,
    /// The persister's physical footprint now and at most over its life.
    pub footprint_bytes: Option<u64>,
    pub footprint_bytes_max: Option<u64>,
}

/// One window the persister sealed.
#[derive(Clone, Debug, PartialEq)]
pub struct Seal {
    pub window: u64,
    pub publication_first: u64,
    pub publication_end: u64,
    pub publications_lost: u64,
    pub events_lost: u64,
    pub sealing_ns: u64,
    pub staged_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SelfReports {
    /// The wall second at which this app started the collector, or none where it did not.
    pub started: Option<u64>,
    /// Counts every change, so a reader copies the reports only when they changed.
    pub revision: u64,
    /// Diagnostic delivery lost reports; this says nothing about saved readings.
    pub incomplete: bool,
    pub ticks: VecDeque<TickReport>,
    pub minutes: VecDeque<MinuteReport>,
    pub seals: VecDeque<Seal>,
    /// Every seal of the run, including those past the bound of `seals`.
    pub seal_totals: SealTotals,
    pending: Pending,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SealTotals {
    pub windows: u64,
    /// Windows of fewer than 60 publications, which stopping or recovery seals.
    pub partial: u64,
    pub publications_lost: u64,
    pub events_lost: u64,
}

#[derive(Clone, Debug, Default)]
enum Pending {
    #[default]
    None,
    /// A ten-second report whose cost and process lines have not arrived yet.
    Tick(Box<TickReport>),
    /// A minute's tick report, whose following lines belong to no ten-second report.
    MinuteTicks(u64),
    /// A minute's cost, whose footprint line follows.
    Minute(MinuteReport),
}

impl SelfReports {
    /// Consumes one bounded output batch; a missing batch reports a delivery discontinuity.
    pub fn output(&mut self, lines: Option<VecDeque<String>>, stream: OutputStream, second: u64) {
        let Some(lines) = lines else {
            if matches!(stream, OutputStream::Stdout) {
                self.pending = Pending::None;
            }
            self.mark_incomplete();
            return;
        };

        for line in lines {
            match stream {
                OutputStream::Stdout => self.standard(&line, second),
                OutputStream::Stderr => self.diagnostic(&line),
            }
        }
    }

    /// Forgets the previous run's reports when this app starts a collector.
    pub fn begin(&mut self, second: u64) {
        *self = Self {
            started: Some(second),
            revision: self.revision.wrapping_add(1),
            ..Self::default()
        };
    }

    /// Reads one line of the collector's standard output.
    pub fn standard(&mut self, line: &str, second: u64) {
        if self.loss(line) {
            return;
        }
        if let Some(rest) = line.strip_prefix("ticks ") {
            self.pending = match tick_header(rest, second) {
                Some(report) => Pending::Tick(Box::new(report)),
                None => Pending::None,
            };
            return;
        }
        if let Some(rest) = line.strip_prefix("minute, ticks ") {
            self.pending = if let Some(second) = minute_ending(rest, self.ticks.back()) {
                Pending::MinuteTicks(second)
            } else {
                self.mark_incomplete();
                Pending::None
            };
            return;
        }
        if let Some(rest) = line.strip_prefix("  ms of CPU a second: ") {
            if let Pending::Tick(report) = &mut self.pending {
                report.sources = sources(rest);
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("  last tick read ") {
            self.standard_processes(rest);
            return;
        }
        if let Some(rest) = line.strip_prefix("recording minute ") {
            self.standard_minute(rest);
            return;
        }
        if let Some(rest) = line.strip_prefix("recording cumulative ") {
            if let Pending::Minute(mut report) = std::mem::take(&mut self.pending) {
                report.footprint_bytes = field(rest, "current_footprint_bytes=");
                report.footprint_bytes_max = field(rest, "lifetime_footprint_bytes_max=");
                push(&mut self.minutes, report, MINUTE_REPORTS_MAX);
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    fn standard_processes(&mut self, text: &str) {
        if !matches!(self.pending, Pending::Tick(_)) {
            return;
        }

        if let Pending::Tick(mut report) = std::mem::take(&mut self.pending) {
            if processes(text, &mut report).is_some() {
                push(&mut self.ticks, *report, TICK_REPORTS_MAX);
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    fn standard_minute(&mut self, text: &str) {
        self.pending = if let Pending::MinuteTicks(second) = std::mem::take(&mut self.pending) {
            match minute(text, second) {
                Some(report) => Pending::Minute(report),
                None => Pending::None,
            }
        } else {
            self.mark_incomplete();
            Pending::None
        };
    }

    fn mark_incomplete(&mut self) {
        if !self.incomplete {
            self.incomplete = true;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn loss(&mut self, line: &str) -> bool {
        let Some(affects_standard) = loss_summary(line) else {
            return false;
        };

        if affects_standard {
            self.pending = Pending::None;
        }
        self.mark_incomplete();
        true
    }

    /// Reads one line of the collector's diagnostic output, which the persister shares.
    pub fn diagnostic(&mut self, line: &str) {
        if self.loss(line) {
            return;
        }
        let Some(rest) = line.strip_prefix("persister sealed ") else {
            return;
        };
        if let Some(seal) = seal(rest) {
            let totals = &mut self.seal_totals;
            totals.windows = totals.windows.saturating_add(1);
            if seal.publication_end.saturating_sub(seal.publication_first) < 60 {
                totals.partial = totals.partial.saturating_add(1);
            }
            totals.publications_lost = totals
                .publications_lost
                .saturating_add(seal.publications_lost);
            totals.events_lost = totals.events_lost.saturating_add(seal.events_lost);
            push(&mut self.seals, seal, SEALS_MAX);
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

/// Whether a valid loss summary invalidates collector stdout assembly.
fn loss_summary(line: &str) -> Option<bool> {
    let (role, rest) = line
        .strip_prefix("diagnostic loss role=")?
        .split_once(' ')?;
    if !matches!(role, "collector" | "persister") {
        return None;
    }
    let (stream, counts) = rest.strip_prefix("stream=")?.split_once(' ')?;
    if !matches!(stream, "stdout" | "stderr") {
        return None;
    }
    let counters = [
        field::<u64>(counts, "queue_full=")?,
        field::<u64>(counts, "staging_contention=")?,
        field::<u64>(counts, "queue_contention=")?,
        field::<u64>(counts, "oversize=")?,
        field::<u64>(counts, "failed_delivery=")?,
    ];
    let setup_status: i32 = field(counts, "setup_status=")?;
    let counts_inexact: u8 = field(counts, "counts_inexact=")?;
    if counts_inexact > 1 {
        return None;
    }
    if counts_inexact == 0 {
        if counters.iter().all(|count| *count == 0) {
            if setup_status == 0 {
                return None;
            }
        }
    }

    Some(role == "collector" && stream == "stdout")
}

fn minute_ending(text: &str, tick: Option<&TickReport>) -> Option<u64> {
    let tick = tick?;
    let (range, _) = text.split_once(": ")?;
    let (first, last) = range.split_once('-')?;
    let first: u64 = first.parse().ok()?;
    let last: u64 = last.parse().ok()?;
    if first > tick.tick_first {
        return None;
    }
    if tick.tick_first > tick.tick_last {
        return None;
    }
    if last != tick.tick_last {
        return None;
    }

    Some(tick.second)
}

fn push<T>(queue: &mut VecDeque<T>, item: T, limit: usize) {
    if queue.len() == limit {
        assert!(queue.pop_front().is_some());
    }
    queue.push_back(item);
    assert!(queue.len() <= limit);
}

/// "1-10: collector CPU 10.01% of a core, …, 149.27 mW, …; ticks late 4.9 ms on average,
/// 10.0 ms at most, 0 slips".
fn tick_header(text: &str, second: u64) -> Option<TickReport> {
    let (range, rest) = text.split_once(": ")?;
    let (first, last) = range.split_once('-')?;
    Some(TickReport {
        tick_first: first.parse().ok()?,
        tick_last: last.parse().ok()?,
        second,
        collector_cpu_percent: number_before(rest, "% of a core")?,
        collector_milliwatts: number_before(rest, " mW")?,
        late_average_ms: number_before(rest, " ms on average")?,
        late_maximum_ms: number_before(rest, " ms at most")?,
        slips: number_before(rest, " slips")?,
        sources: Vec::new(),
        processes_observed: 0,
        processes_refused: 0,
        processes_gone: 0,
        event_drops: 0,
    })
}

/// "processes 8.05, coalitions 2.47, …, unattributed 1.28".
fn sources(text: &str) -> Vec<(String, f32)> {
    let mut result = Vec::with_capacity(SOURCES_MAX);
    for part in text.split(", ").take(SOURCES_MAX) {
        let Some((name, value)) = part.rsplit_once(' ') else {
            continue;
        };
        if let Ok(value) = value.parse::<f32>() {
            result.push((name.to_owned(), value));
        }
    }
    result
}

/// "757 processes (360 refused, 0 gone), …; 0 starts, 0 execs and 0 exits dropped from a full
/// event list".
fn processes(text: &str, report: &mut TickReport) -> Option<()> {
    let (counts, _) = text.split_once("),")?;
    report.processes_observed = number_before(counts, " processes (")?;
    report.processes_refused = number_before(counts, " refused")?;
    report.processes_gone = number_before(counts, " gone")?;
    let (_, drops) = text.rsplit_once("; ")?;
    let starts: u64 = number_before(drops, " starts")?;
    let execs: u64 = number_before(drops, " execs")?;
    let exits: u64 = number_before(drops, " exits dropped")?;
    report.event_drops = starts.checked_add(execs)?.checked_add(exits)?;
    Some(())
}

/// `elapsed_ns=60015526708 collector_cpu_ns=… persister_cpu_ns=… combined_percent=3.227186`.
fn minute(text: &str, second: u64) -> Option<MinuteReport> {
    Some(MinuteReport {
        second,
        elapsed_ns: field(text, "elapsed_ns=")?,
        collector_cpu_ns: field(text, "collector_cpu_ns=")?,
        persister_cpu_ns: field(text, "persister_cpu_ns=")?,
        combined_percent: field(text, "combined_percent=")?,
        footprint_bytes: None,
        footprint_bytes_max: None,
    })
}

/// `window=0 publications=1..61 loss=0 events_lost=0 seal_ns=281035875 staged_bytes=91129424`.
fn seal(text: &str) -> Option<Seal> {
    let publications = text.split_once("publications=")?.1;
    let publications = publications.split(' ').next()?;
    let (first, end) = publications.split_once("..")?;
    Some(Seal {
        window: field(text, "window=")?,
        publication_first: first.parse().ok()?,
        publication_end: end.parse().ok()?,
        publications_lost: field(text, "loss=")?,
        events_lost: field(text, "events_lost=")?,
        sealing_ns: field(text, "seal_ns=")?,
        staged_bytes: field(text, "staged_bytes=")?,
    })
}

/// The value of `key`, which runs to the next space. A key must not end another key, so each
/// match starts the text or follows a space.
fn field<T: std::str::FromStr>(text: &str, key: &str) -> Option<T> {
    for part in text.split(' ') {
        if let Some(value) = part.strip_prefix(key) {
            return value.parse().ok();
        }
    }
    None
}

/// The number written just before the first `marker`.
fn number_before<T: std::str::FromStr>(text: &str, marker: &str) -> Option<T> {
    let (before, _) = text.split_once(marker)?;
    let start = before
        .rfind(|character: char| !(character.is_ascii_digit() || character == '.'))
        .map_or(0, |index| index + 1);
    before[start..].parse().ok()
}

#[cfg(test)]
#[path = "self_reports_tests.rs"]
mod tests;
