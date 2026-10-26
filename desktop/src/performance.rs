use std::time::{Duration, Instant};

use gpui::{Context, Window};

pub fn start_deadline(
    entity: &gpui::Entity<crate::dashboard::Dashboard>,
    cx: &mut gpui::App,
) -> Result<(), String> {
    let performance = &entity.read(cx).performance;
    let Some(seconds) = performance.options.seconds else {
        return Ok(());
    };
    let remaining = Duration::from_secs(u64::from(seconds))
        .saturating_sub(performance.started.elapsed())
        .max(Duration::from_millis(1));
    let view = entity.downgrade();
    let application = cx.to_async();
    let timer = crate::live_timer::Timer::with_interval(remaining, move || {
        application.update(|cx| {
            if let Err(error) = view.update(cx, |dashboard, cx| {
                dashboard.performance.report();
                cx.quit();
            }) {
                eprintln!("Timed view shutdown: {error}");
            }
        });
    })?;
    entity.update(cx, |dashboard, _| {
        dashboard.performance.deadline_timer = Some(timer);
    });
    Ok(())
}

/// The page a live window opens on when neither `--page` nor the user chose one.
pub const DEFAULT_PAGE: &str = "overview";

pub struct Options {
    pub source: Source,
    /// The page `--page` named, which opens over the page the user left open.
    pub named_page: Option<String>,
    pub scrub: bool,
    pub hardware: bool,
    /// The minutes `--minutes` named, if any.
    pub named_minutes: Option<u16>,
    pub seconds: Option<u32>,
    pub replay: Option<std::path::PathBuf>,
    /// The control socket an automation build listens on, with its window hidden.
    pub control: Option<std::path::PathBuf>,
    mode: RunMode,
}

#[derive(Default)]
pub struct Source {
    pub reference: bool,
    pub recording: Option<std::path::PathBuf>,
    pub probe: ProbeMode,
    pub start_collector: bool,
    #[cfg(feature = "automation")]
    attach_live: bool,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum ProbeMode {
    #[default]
    None,
    Check,
    Soak,
}

#[derive(Clone, Copy, PartialEq)]
enum RunMode {
    Interactive,
    Check,
    Smoke,
}

impl Options {
    /// The page `--page` named, else the default page.
    pub fn page(&self) -> &str {
        self.named_page.as_deref().unwrap_or(DEFAULT_PAGE)
    }

    pub fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let arguments: Vec<_> = std::env::args().skip(1).take(21).collect();
        Self::parse_arguments(&arguments)
    }

    pub(crate) fn parse_arguments(
        arguments: &[String],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            source: Source::default(),
            named_page: None,
            scrub: false,
            hardware: false,
            named_minutes: None,
            seconds: None,
            replay: None,
            control: None,
            mode: RunMode::Interactive,
        };
        if arguments.len() > 20 {
            return Err("too many arguments".into());
        }
        let mut position = 0;
        for _ in 0..20 {
            if position == arguments.len() {
                break;
            }
            position = options.parse_argument(arguments, position)?;
        }
        options.validate_replay()?;
        options.validate_seconds()?;
        #[cfg(feature = "automation")]
        options.source.validate_attach()?;
        Ok(options)
    }

    pub fn check(&self) -> bool {
        self.mode == RunMode::Check
    }

    pub fn smoke(&self) -> bool {
        self.mode == RunMode::Smoke
    }

    fn validate_seconds(&self) -> Result<(), &'static str> {
        if self.source.probe == ProbeMode::Soak {
            if self.source.reference {
                return Err("--live-soak cannot use --reference");
            }
            if self.mode != RunMode::Interactive {
                return Err("--live-soak cannot use --check or --smoke");
            }
        }

        if let Some(seconds) = self.seconds {
            if self.interactive_live() {
                if !(1..=1800).contains(&seconds) {
                    return Err("Live window seconds must be 1..1800");
                }

                return Ok(());
            }

            return validate_duration(seconds, self.source.probe == ProbeMode::Soak);
        }

        Ok(())
    }

    fn interactive_live(&self) -> bool {
        if self.source.reference {
            return false;
        }

        if self.source.probe != ProbeMode::None {
            return false;
        }

        self.mode == RunMode::Interactive
    }

    fn validate_replay(&self) -> Result<(), &'static str> {
        if self.replay.is_none() {
            return Ok(());
        }
        if !self.source.reference {
            return Err("--replay requires --reference");
        }
        if self.source.probe != ProbeMode::None {
            return Err("--replay cannot run with --live-check");
        }
        if self.seconds.is_some() {
            return Err("--replay duration comes from its action file");
        }
        if self.scrub {
            return Err("--replay cannot run with --scrub");
        }
        if self.smoke() {
            return Err("--replay cannot run with --smoke");
        }
        Ok(())
    }

    fn parse_argument(
        &mut self,
        arguments: &[String],
        mut position: usize,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        assert!(position < arguments.len());
        let flag = &arguments[position];
        position += 1;
        match flag.as_str() {
            "--page" => {
                self.named_page = Some(
                    arguments
                        .get(position)
                        .ok_or("--page needs a route")?
                        .clone(),
                );
                position += 1;
            }
            "--scrub" => self.scrub = true,
            "--hardware" => self.hardware = true,
            "--replay" => {
                self.replay = Some(
                    arguments
                        .get(position)
                        .ok_or("--replay needs a path")?
                        .into(),
                );
                position += 1;
            }
            "--control" => {
                self.control = Some(
                    arguments
                        .get(position)
                        .ok_or("--control needs a socket path")?
                        .into(),
                );
                position += 1;
            }
            "--smoke" => self.parse_mode(RunMode::Smoke)?,
            "--check" => self.parse_mode(RunMode::Check)?,
            "--minutes" => {
                let minutes = arguments
                    .get(position)
                    .ok_or("--minutes needs a value")?
                    .parse()?;
                if ![15, 60].contains(&minutes) {
                    return Err("minutes must be 15 or 60".into());
                }
                self.named_minutes = Some(minutes);
                position += 1;
            }
            "--seconds" => {
                let seconds = arguments
                    .get(position)
                    .ok_or("--seconds needs a value")?
                    .parse()?;
                self.seconds = Some(seconds);
                position += 1;
            }
            "--help" => parse_argument_help(),
            _ => return self.source.parse_argument(flag, arguments, position),
        }
        Ok(position)
    }

    fn parse_mode(&mut self, mode: RunMode) -> Result<(), &'static str> {
        if self.mode != RunMode::Interactive {
            if self.mode != mode {
                return Err("--check and --smoke select different run modes");
            }
        }
        self.mode = mode;
        Ok(())
    }
}

impl Source {
    #[cfg(feature = "automation")]
    fn validate_attach(&self) -> Result<(), &'static str> {
        if self.attach_live {
            if self.recording.is_none() {
                return Err("--attach-live requires --recording");
            }
            if self.start_collector {
                return Err("--attach-live cannot start a collector");
            }
        }
        Ok(())
    }

    /// Whether the named recording uses only saved readings.
    pub(crate) fn read_only(&self) -> bool {
        if self.recording.is_none() {
            return false;
        }
        if self.start_collector {
            return false;
        }
        #[cfg(feature = "automation")]
        if self.attach_live {
            return false;
        }
        true
    }

    fn parse_argument(
        &mut self,
        flag: &str,
        arguments: &[String],
        mut position: usize,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        match flag {
            "--reference" => self.reference = true,
            "--live-check" => self.select_probe(ProbeMode::Check)?,
            "--live-soak" => self.select_probe(ProbeMode::Soak)?,
            "--start-collector" => self.start_collector = true,
            #[cfg(feature = "automation")]
            "--attach-live" => self.attach_live = true,
            "--recording" => {
                self.recording = Some(
                    arguments
                        .get(position)
                        .ok_or("--recording needs a path")?
                        .into(),
                );
                position += 1;
            }
            _ => return Err(format!("unknown flag {flag}").into()),
        }
        Ok(position)
    }

    fn select_probe(&mut self, probe: ProbeMode) -> Result<(), &'static str> {
        if self.probe != ProbeMode::None {
            if self.probe != probe {
                return Err("Select only one of --live-check and --live-soak");
            }
        }
        self.probe = probe;
        Ok(())
    }
}

fn validate_duration(seconds: u32, soak: bool) -> Result<(), &'static str> {
    if soak {
        if !(5..=1800).contains(&seconds) {
            return Err("Live soak seconds must be 5..1800");
        }
    } else {
        if !(1..=300).contains(&seconds) {
            return Err("seconds must be 1..300");
        }
    }
    Ok(())
}

pub struct Performance {
    pub options: Options,
    pub frames: usize,
    pub started: Instant,
    previous: Instant,
    intervals: Vec<f64>,
    deadline_timer: Option<crate::live_timer::Timer>,
}

impl Performance {
    pub fn new(options: Options) -> Self {
        let started = Instant::now();
        Self {
            options,
            frames: 0,
            started,
            previous: started,
            intervals: Vec::with_capacity(40_000),
            deadline_timer: None,
        }
    }

    pub fn frame<T: 'static>(&mut self, window: &mut Window, cx: &mut Context<T>) {
        let now = Instant::now();
        if self.frames > 10 {
            if self.intervals.len() < 40_000 {
                self.intervals
                    .push(now.duration_since(self.previous).as_secs_f64() * 1000.0);
            }
        }
        self.previous = now;
        self.frames += 1;
        if self.options.scrub {
            window.request_animation_frame();
        } else {
            if self.options.smoke() {
                window.request_animation_frame();
            }
        }
        if let Some(seconds) = self.options.seconds {
            if self.started.elapsed() >= Duration::from_secs(u64::from(seconds)) {
                self.report();
                cx.quit();
            }
        }
    }

    fn report(&mut self) {
        self.intervals.sort_by(f64::total_cmp);
        let percentile = |percent: usize| {
            self.intervals
                .get(self.intervals.len().saturating_sub(1) * percent / 100)
                .copied()
        };
        println!(
            "{}",
            serde_json::json!({
                "page": self.options.page(), "scrub": self.options.scrub,
                "elapsed_seconds": self.started.elapsed().as_secs_f64(),
                "render_count": self.frames, "measured_intervals": self.intervals.len(),
                "render_interval_ms_p50": percentile(50), "render_interval_ms_p95": percentile(95),
                "render_interval_ms_p99": percentile(99),
                "note": concat!(
                    "CPU-side view render intervals after ten warmup renders; ",
                    "not GPU presentation latency"
                )
            })
        );
    }
}

/// Prints the flags and ends the process, as `--help` asks.
fn parse_argument_help() -> ! {
    println!(
        "flightdeck [--recording PATH] [--start-collector] [--page ROUTE] [--minutes 15|60]\n\
         [--seconds 1..1800] [--live-check --seconds 5..300]\n\
         [--reference [--scrub]\n\
         [--hardware] [--smoke] [--minutes 15|60] [--seconds 1..300]\n\
         [--replay ACTIONS.json]] [--check]\n\
         [--control SOCKET] (automation builds: hidden window, socket commands)"
    );
    println!("--live-soak [--recording PATH] [--seconds 5..1800; default 900]");
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::{Options, ProbeMode};

    #[cfg(feature = "automation")]
    #[test]
    fn attaching_live_requires_a_recording_and_excludes_starting_a_collector() {
        // Goal: a profiling attachment cannot silently start a collector.
        // Method: parse the valid attachment and both flag orders for conflicting options.
        let valid = ["--attach-live", "--recording", "/fixture"].map(str::to_owned);
        let attached = Options::parse_arguments(&valid).unwrap();
        assert!(!attached.source.read_only());
        assert!(!attached.source.start_collector);
        let missing = ["--attach-live"].map(str::to_owned);
        assert!(Options::parse_arguments(&missing).is_err());
        for invalid in [
            [
                "--attach-live",
                "--recording",
                "/fixture",
                "--start-collector",
            ],
            [
                "--start-collector",
                "--recording",
                "/fixture",
                "--attach-live",
            ],
        ] {
            assert!(Options::parse_arguments(&invalid.map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn parses_the_control_socket_and_refuses_it_without_a_path() {
        // Goal: the control socket path reaches the launch, and a missing path is an error.
        // Method: parse the flag with a path, then as the last argument without one.
        let arguments = ["--start-collector", "--control", "/tmp/fd.sock"].map(str::to_owned);
        let missing = ["--control"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.control, Some("/tmp/fd.sock".into()));
        assert!(Options::parse_arguments(&missing).is_err());
    }

    #[test]
    fn parses_a_long_soak_when_the_mode_precedes_its_duration() {
        // The sustained control accepts 1800 seconds through the actual CLI parser.
        let arguments = ["--live-soak", "--seconds", "1800"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1800));
        assert!(options.source.probe == ProbeMode::Soak);
    }

    #[test]
    fn parses_a_long_soak_when_the_duration_precedes_its_mode() {
        // A valid duration must not depend on which flag arrives first.
        let arguments = ["--seconds", "1800", "--live-soak"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1800));
        assert!(options.source.probe == ProbeMode::Soak);
    }

    #[test]
    fn accepts_the_shortest_live_soak() {
        // Five seconds permits a bounded startup/lifecycle control.
        let arguments = ["--live-soak", "--seconds", "5"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(5));
    }

    #[test]
    fn rejects_durations_outside_the_live_soak_contract() {
        // Literal adjacent invalid boundaries catch accidental changes to either limit.
        for duration in ["4", "1801"] {
            let arguments = ["--live-soak", "--seconds", duration].map(str::to_owned);

            assert!(Options::parse_arguments(&arguments).is_err());
        }
    }

    #[test]
    fn rejects_conflicting_probe_modes_in_either_order() {
        // Check and soak cannot silently select the last flag's meaning.
        for arguments in [
            ["--live-soak", "--live-check"],
            ["--live-check", "--live-soak"],
        ] {
            let arguments = arguments.map(str::to_owned);

            assert!(Options::parse_arguments(&arguments).is_err());
        }
    }

    #[test]
    fn rejects_reference_and_reference_checks_in_live_soak_mode() {
        // A headless live control must not dispatch a reference window or fixture check.
        for flag in ["--reference", "--check", "--smoke"] {
            for arguments in [["--live-soak", flag], [flag, "--live-soak"]] {
                let arguments = arguments.map(str::to_owned);

                assert!(Options::parse_arguments(&arguments).is_err());
            }
        }
    }

    #[test]
    fn retains_the_captured_timed_window_limit() {
        // A longer live observation must not change the captured reference workload's limit.
        let valid = ["--reference", "--seconds", "300"].map(str::to_owned);
        let invalid = ["--reference", "--seconds", "301"].map(str::to_owned);

        assert!(Options::parse_arguments(&valid).is_ok());
        assert!(Options::parse_arguments(&invalid).is_err());
    }

    #[test]
    fn permits_a_live_window_to_observe_the_full_fifteen_minute_history() {
        // The real CLI must permit an uninterrupted observation across the retention interval.
        let arguments = ["--seconds", "900"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(900));
    }

    #[test]
    fn permits_a_thirty_minute_live_window_for_a_second_retention_interval() {
        // Two history intervals let the observer separate retained context from continued growth.
        let arguments = ["--seconds", "1800", "--page", "overview"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1800));
    }

    #[test]
    fn rejects_a_zero_second_live_window() {
        // The actual parser rejects the adjacent invalid live duration.
        let arguments = ["--seconds", "0"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn rejects_a_live_window_beyond_thirty_minutes() {
        // The actual parser rejects the adjacent invalid live duration.
        let arguments = ["--seconds", "1801"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn permits_a_one_second_live_window_for_a_bounded_startup_control() {
        // Short interactive controls keep their existing minimum while sustained runs grow.
        let arguments = ["--seconds", "1"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1));
    }

    #[test]
    fn rejects_301_seconds_when_reference_comes_before_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--reference", "--seconds", "301"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn rejects_301_seconds_when_reference_comes_after_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--seconds", "301", "--reference"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn accepts_one_second_in_reference_mode() {
        // The actual parser preserves the constrained mode's shortest timed observation.
        let arguments = ["--reference", "--seconds", "1"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1));
    }

    #[test]
    fn rejects_301_seconds_when_live_check_comes_before_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--live-check", "--seconds", "301"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn rejects_301_seconds_when_live_check_comes_after_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--seconds", "301", "--live-check"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn accepts_one_second_in_live_check_mode() {
        // The actual parser preserves the constrained mode's shortest timed observation.
        let arguments = ["--live-check", "--seconds", "1"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1));
    }

    #[test]
    fn rejects_301_seconds_when_check_comes_before_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--check", "--seconds", "301"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn rejects_301_seconds_when_check_comes_after_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--seconds", "301", "--check"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn accepts_one_second_in_check_mode() {
        // The actual parser preserves the constrained mode's shortest timed observation.
        let arguments = ["--check", "--seconds", "1"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1));
    }

    #[test]
    fn rejects_301_seconds_when_smoke_comes_before_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--smoke", "--seconds", "301"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn rejects_301_seconds_when_smoke_comes_after_duration() {
        // The actual parser retains the constrained mode's upper boundary in this order.
        let arguments = ["--seconds", "301", "--smoke"].map(str::to_owned);

        assert!(Options::parse_arguments(&arguments).is_err());
    }

    #[test]
    fn accepts_one_second_in_smoke_mode() {
        // The actual parser preserves the constrained mode's shortest timed observation.
        let arguments = ["--smoke", "--seconds", "1"].map(str::to_owned);

        let options = Options::parse_arguments(&arguments).unwrap();

        assert_eq!(options.seconds, Some(1));
    }
}
