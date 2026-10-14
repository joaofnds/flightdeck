use std::ffi::c_int;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use collector::clock::Timebase;

const WNOHANG: c_int = 1;
const KERN_SUCCESS: c_int = 0;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const REAP_EVERY_NS: u64 = 10_000_000;

/// The fastest rate a run takes, a fork a nanosecond, so the period between forks floors to
/// a nanosecond at least.
const PER_SECOND_MAX: u64 = NANOSECONDS_PER_SECOND;

/// The longest a child sleeps, the most milliseconds whose microseconds fit the u32 usleep
/// takes.
const CHILD_MS_MAX: u32 = u32::MAX / 1_000;

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_wait_until(deadline: u64) -> c_int;
    fn fork() -> c_int;
    fn _exit(status: c_int) -> !;
    fn usleep(microseconds: u32) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
}

fn main() -> ExitCode {
    let burst = match collector::arguments("forkrate").and_then(|arguments| burst_from(&arguments))
    {
        Ok(burst) => burst,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };

    match run(&burst) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("forkrate: {error}");
            ExitCode::FAILURE
        }
    }
}

/// A run of `forks` children over `seconds`, `per_second` of them a second, each sleeping
/// `child_ms` before it exits, with their pids written to the file `pids` names.
#[derive(Debug, PartialEq)]
struct Burst {
    per_second: u64,
    seconds: u64,
    forks: u32,
    child_ms: u32,
    pids: Option<String>,
}

fn burst_from(arguments: &[String]) -> Result<Burst, String> {
    let usage = || "usage: forkrate PER_SECOND SECONDS CHILD_MS [--pids FILE]".to_owned();
    let (per_second, seconds, child_ms, pids) = match arguments {
        [per_second, seconds, child_ms] => (per_second, seconds, child_ms, None),
        [per_second, seconds, child_ms, flag, file] if flag == "--pids" => {
            (per_second, seconds, child_ms, Some(file.clone()))
        }
        _ => return Err(usage()),
    };
    let per_second: u64 = per_second.parse().map_err(|_| usage())?;
    let seconds: u64 = seconds.parse().map_err(|_| usage())?;
    let child_ms: u32 = child_ms.parse().map_err(|_| usage())?;

    if per_second == 0 {
        return Err("forkrate: a rate of 0 forks a second forks nothing".to_owned());
    }
    if seconds == 0 {
        return Err("forkrate: a run of 0 seconds forks nothing".to_owned());
    }
    if per_second > PER_SECOND_MAX {
        return Err(format!(
            "forkrate: a rate of {per_second} forks a second passes one a nanosecond"
        ));
    }
    if child_ms > CHILD_MS_MAX {
        return Err(format!(
            "forkrate: a child time of {child_ms} ms passes the {CHILD_MS_MAX} usleep takes"
        ));
    }
    let Some(forks) = per_second.checked_mul(seconds) else {
        return Err(format!(
            "forkrate: {seconds} seconds at {per_second} a second pass a u64"
        ));
    };
    let Ok(forks) = u32::try_from(forks) else {
        return Err(format!(
            "forkrate: {forks} forks pass the {} a run takes",
            u32::MAX
        ));
    };

    assert!(per_second > 0);
    assert!(seconds > 0);
    assert!(forks > 0);
    assert_eq!(u64::from(forks), per_second * seconds);
    Ok(Burst {
        per_second,
        seconds,
        forks,
        child_ms,
        pids,
    })
}

fn run(burst: &Burst) -> io::Result<()> {
    let timebase = Timebase::read()?;
    let start = unsafe { mach_absolute_time() };
    let forked = run_forks(burst, timebase, start)?;
    let forks = forked.forks();
    let rest = reap_rest(forks - forked.reaped.count)?;
    let end = unsafe { mach_absolute_time() };

    let mut reaped = forked.reaped;
    reaped.add(&rest);
    assert_eq!(forks, reaped.count);
    let Some(elapsed_ticks) = end.checked_sub(start) else {
        return Err(io::Error::other(format!(
            "mach_absolute_time went back from {start} to {end}"
        )));
    };
    let elapsed = Duration::from_nanos(timebase.nanoseconds(elapsed_ticks)?);

    let reached: Vec<String> = forked
        .per_second_reached
        .iter()
        .map(u64::to_string)
        .collect();
    let mut out = io::stdout().lock();
    writeln!(out, "forks each second: {}", reached.join(" "))?;
    writeln!(
        out,
        "{forks} of {} forks in {:.1} s, {:.0} a second, {} late, {} reaped, {} of them failed",
        burst.forks,
        elapsed.as_secs_f64(),
        f64::from(forks) / elapsed.as_secs_f64(),
        forked.late,
        reaped.count,
        reaped.failed,
    )?;

    if let Some(file) = &burst.pids {
        run_write_pids(file, &forked.children)?;
    }
    if reaped.failed > 0 {
        return Err(io::Error::other(format!(
            "{} children exited with a failure, such as a sleep that failed",
            reaped.failed
        )));
    }
    if forks < burst.forks {
        return Err(io::Error::other(format!(
            "forked {forks} of {} children in the run's {} seconds, since forking fell behind \
             the rate",
            burst.forks, burst.seconds
        )));
    }
    Ok(())
}

/// What forking a run's children came to.
struct Forked {
    children: Vec<i32>,
    per_second_reached: Vec<u64>,
    late: u32,
    reaped: Reaped,
}

impl Forked {
    /// The children the run forked, short of those it asked for where forking fell behind.
    fn forks(&self) -> u32 {
        // A run forks at most the u32 of children its burst asks for.
        match u32::try_from(self.children.len()) {
            Ok(forks) => forks,
            Err(error) => unreachable!("{error}"),
        }
    }
}

/// Forks the run's children at its rate from `start` until it has forked them all or its
/// seconds have passed, reaping those that have exited every 10 ms, and counts the forks each
/// second reached and those that came late.
fn run_forks(burst: &Burst, timebase: Timebase, start: u64) -> io::Result<Forked> {
    // The period floors to the nanosecond, which a rate of a fork a nanosecond at most keeps
    // at 1 or more.
    let period_ns = NANOSECONDS_PER_SECOND / burst.per_second;
    assert!(period_ns > 0);
    let period = timebase.ticks(period_ns)?;
    let second = timebase.ticks(NANOSECONDS_PER_SECOND)?;
    let reap_every = timebase.ticks(REAP_EVERY_NS)?;
    // A run forks at least once a second, so it runs no more seconds than a u32 of forks,
    // whose nanoseconds fit a u64.
    assert!(burst.seconds <= u64::from(burst.forks));
    let run = timebase.ticks(burst.seconds * NANOSECONDS_PER_SECOND)?;

    const { assert!(size_of::<u32>() <= size_of::<usize>()) };
    let capacity = match usize::try_from(burst.forks) {
        Ok(capacity) => capacity,
        Err(error) => unreachable!("{error}"),
    };
    let mut forked = Forked {
        children: Vec::with_capacity(capacity),
        per_second_reached: Vec::new(),
        late: 0,
        reaped: Reaped::default(),
    };
    let mut next = start;
    let mut next_reap = start + reap_every;
    let mut second_ends = start + second;
    let run_ends = start + run;
    let mut this_second = 0_u64;
    for fork in 0..burst.forks {
        forked.children.push(fork_child(burst.child_ms)?);
        this_second += 1;
        next += period;

        let now = unsafe { mach_absolute_time() };
        if now >= next_reap {
            forked.reaped.add(&reap(fork + 1 - forked.reaped.count)?);
            next_reap += reap_every;
        }
        if now >= second_ends {
            forked.per_second_reached.push(this_second);
            this_second = 0;
            second_ends += second;
        }
        if now > next {
            forked.late += 1;
        }
        if now >= run_ends {
            // Forking fell behind the rate. Ending on time keeps the run inside its seconds,
            // which bound how long followprobe waits for it.
            break;
        }

        wait_until(next)?;
    }
    forked.per_second_reached.push(this_second);

    assert!(forked.children.len() <= capacity);
    assert!(forked.reaped.count <= forked.forks());
    Ok(forked)
}

fn run_write_pids(file: &str, children: &[i32]) -> io::Result<()> {
    let mut out = io::BufWriter::new(std::fs::File::create(file)?);
    for pid in children {
        writeln!(out, "{pid}")?;
    }
    out.flush()
}

/// Forks a child that sleeps `child_ms` and exits, and returns its pid.
fn fork_child(child_ms: u32) -> io::Result<i32> {
    assert!(child_ms <= CHILD_MS_MAX);
    const { assert!(CHILD_MS_MAX.checked_mul(1_000).is_some()) };
    let microseconds = child_ms * 1_000;

    let pid = unsafe { fork() };
    if pid == 0 {
        // The child's exit status carries what usleep returned, 0 or -1, so the parent
        // counts a failed sleep.
        let slept = if microseconds > 0 {
            unsafe { usleep(microseconds) }
        } else {
            0
        };
        unsafe { _exit(slept) };
    }
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }

    assert!(pid > 0);
    Ok(pid)
}

fn wait_until(deadline: u64) -> io::Result<()> {
    let returned = unsafe { mach_wait_until(deadline) };
    if returned != KERN_SUCCESS {
        return Err(io::Error::other(format!(
            "mach_wait_until returned {returned:#x}"
        )));
    }
    Ok(())
}

/// The children reaped, and how many of them exited with a failure.
#[derive(Debug, Default, PartialEq)]
struct Reaped {
    count: u32,
    failed: u32,
}

impl Reaped {
    /// Counts a child that exited with `status`, as a failure where it is not 0.
    fn add_exit(&mut self, status: c_int) {
        self.count += 1;
        if status != 0 {
            self.failed += 1;
        }
        assert!(self.failed <= self.count);
    }

    fn add(&mut self, other: &Reaped) {
        self.count += other.count;
        self.failed += other.failed;
        assert!(self.failed <= self.count);
    }
}

/// Reaps the children that have exited, at most `outstanding`, without waiting for one.
fn reap(outstanding: u32) -> io::Result<Reaped> {
    assert!(outstanding > 0);

    let mut reaped = Reaped::default();
    while reaped.count < outstanding {
        let mut status: c_int = 0;
        let pid = unsafe { waitpid(-1, &raw mut status, WNOHANG) };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            // The children still running have not exited yet.
            break;
        }
        reaped.add_exit(status);
    }

    assert!(reaped.count <= outstanding);
    Ok(reaped)
}

/// Waits for the `outstanding` children still running to exit, and reaps each.
fn reap_rest(outstanding: u32) -> io::Result<Reaped> {
    let mut reaped = Reaped::default();
    while reaped.count < outstanding {
        let mut status: c_int = 0;
        let pid = unsafe { waitpid(-1, &raw mut status, 0) };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "waitpid returned no child though it was asked to wait for one",
            ));
        }
        reaped.add_exit(status);
    }

    assert_eq!(outstanding, reaped.count);
    Ok(reaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    #[test]
    fn takes_the_rate_the_run_and_the_child_time() {
        // Goal: a run forks its rate times its seconds, each child sleeping the time given.
        // Method: parse 300 forks a second for 15 seconds, children sleeping 6 ms.
        assert_eq!(
            Ok(Burst {
                per_second: 300,
                seconds: 15,
                forks: 4_500,
                child_ms: 6,
                pids: None,
            }),
            burst_from(&arguments(&["300", "15", "6"]))
        );
    }

    #[test]
    fn takes_a_file_for_the_pids() {
        // Goal: --pids names the file the children's pids go to.
        // Method: parse a run with --pids pids.txt.
        let burst = burst_from(&arguments(&["300", "15", "6", "--pids", "pids.txt"]));

        assert_eq!(Some("pids.txt".to_owned()), burst.unwrap().pids);
    }

    #[test]
    fn takes_the_longest_child_time_usleep_takes() {
        // Goal: the longest child time, whose microseconds still fit a u32, is taken.
        // Method: parse a child time of CHILD_MS_MAX.
        let child_ms = CHILD_MS_MAX.to_string();

        let burst = burst_from(&arguments(&["300", "15", &child_ms]));

        assert_eq!(CHILD_MS_MAX, burst.unwrap().child_ms);
    }

    #[test]
    fn refuses_an_unknown_flag() {
        // Goal: a flag forkrate does not take is a usage error.
        // Method: parse a run with --pid in place of --pids.
        assert!(burst_from(&arguments(&["300", "15", "6", "--pid", "pids.txt"])).is_err());
    }

    #[test]
    fn refuses_a_rate_of_zero() {
        // Goal: a rate of no forks a second is a usage error, not a rate of one.
        // Method: parse a rate of 0.
        assert!(burst_from(&arguments(&["0", "15", "6"])).is_err());
    }

    #[test]
    fn refuses_a_run_of_no_seconds() {
        // Goal: a run of no seconds is a usage error, not a run that forks nothing.
        // Method: parse a run of 0 seconds.
        assert!(burst_from(&arguments(&["300", "0", "6"])).is_err());
    }

    #[test]
    fn refuses_a_rate_past_a_fork_a_nanosecond() {
        // Goal: a rate whose period floors to no nanoseconds is a usage error.
        // Method: parse a rate one past a billion a second.
        assert!(burst_from(&arguments(&["1000000001", "1", "0"])).is_err());
    }

    #[test]
    fn refuses_a_run_whose_forks_pass_a_u64() {
        // Goal: a rate and a run whose product overflows are a usage error, not a crash.
        // Method: parse a run of u64::MAX seconds at 2 forks a second.
        assert!(burst_from(&arguments(&["2", "18446744073709551615", "0"])).is_err());
    }

    #[test]
    fn refuses_a_run_of_more_forks_than_a_u32_holds() {
        // Goal: a run of more forks than a u32 holds is a usage error, since the report's
        // rate takes the forks as a u32.
        // Method: parse 5 seconds at a billion forks a second.
        assert!(burst_from(&arguments(&["1000000000", "5", "0"])).is_err());
    }

    #[test]
    fn refuses_a_child_time_past_what_usleep_takes() {
        // Goal: a child time whose microseconds pass usleep's u32 is a usage error, not a
        // sleep of u32::MAX microseconds.
        // Method: parse a child time of 4,294,968 ms, the first past u32::MAX microseconds.
        assert!(burst_from(&arguments(&["300", "15", "4294968"])).is_err());
    }

    #[test]
    fn counts_a_child_that_exited_cleanly() {
        // Goal: a child that exits with status 0 is reaped and not failed.
        // Method: add an exit of status 0.
        let mut reaped = Reaped::default();

        reaped.add_exit(0);

        assert_eq!(
            Reaped {
                count: 1,
                failed: 0
            },
            reaped
        );
    }

    #[test]
    fn counts_a_child_whose_sleep_failed_as_failed() {
        // Goal: a child that exits with usleep's -1, which waitpid gives as 0xff00, is
        // reaped and failed.
        // Method: add an exit of status 0xff00.
        let mut reaped = Reaped::default();

        reaped.add_exit(0xff00);

        assert_eq!(
            Reaped {
                count: 1,
                failed: 1
            },
            reaped
        );
    }
}
