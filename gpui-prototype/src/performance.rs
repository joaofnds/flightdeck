use std::time::{Duration, Instant};

use gpui::{Context, Window};

pub struct Options {
    pub page: String,
    pub scrub: bool,
    pub animate: bool,
    pub hardware: bool,
    pub range_minutes: u16,
    pub seconds: Option<u32>,
    mode: RunMode,
}

#[derive(Clone, Copy, PartialEq)]
enum RunMode {
    Interactive,
    Check,
    Smoke,
}

impl Options {
    pub fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            page: "overview".into(),
            scrub: false,
            animate: false,
            hardware: false,
            range_minutes: 15,
            seconds: None,
            mode: RunMode::Interactive,
        };
        let arguments: Vec<_> = std::env::args().skip(1).collect();
        if arguments.len() > 12 {
            return Err("too many arguments".into());
        }
        let mut position = 0;
        for _ in 0..12 {
            if position == arguments.len() {
                break;
            }
            position = options.parse_argument(&arguments, position)?;
        }
        if options.seconds.is_some() {
            if !options.scrub {
                if !options.animate {
                    if !options.smoke() {
                        return Err("--seconds requires --scrub, --animate, or --smoke".into());
                    }
                }
            }
        }
        if options.animate {
            if !options.hardware {
                return Err("--animate requires --hardware".into());
            }
            if options.page != "overview" {
                return Err("animation is on the overview page".into());
            }
        }
        Ok(options)
    }

    pub fn check(&self) -> bool {
        self.mode == RunMode::Check
    }

    pub fn smoke(&self) -> bool {
        self.mode == RunMode::Smoke
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
                self.page
                    .clone_from(arguments.get(position).ok_or("--page needs a route")?);
                position += 1;
            }
            "--scrub" => self.scrub = true,
            "--animate" => self.animate = true,
            "--hardware" => self.hardware = true,
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
                self.range_minutes = minutes;
                position += 1;
            }
            "--seconds" => {
                let seconds = arguments
                    .get(position)
                    .ok_or("--seconds needs a value")?
                    .parse()?;
                if !(1..=300).contains(&seconds) {
                    return Err("seconds must be 1..300".into());
                }
                self.seconds = Some(seconds);
                position += 1;
            }
            "--help" => {
                println!(
                    "flightdeck-gpui [--page ROUTE] [--scrub] [--animate] \
                        [--hardware] [--smoke] [--minutes 15|60] [--seconds 1..300] [--check]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown flag {flag}").into()),
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

pub struct Performance {
    pub options: Options,
    pub frames: usize,
    pub started: Instant,
    previous: Instant,
    intervals: Vec<f64>,
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
                "page": self.options.page, "scrub": self.options.scrub,
                "animate": self.options.animate,
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
