use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use collector::clock::Timebase;

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_wait_until(deadline: u64) -> libc::c_int;
}

struct Burst {
    per_second: u64,
    seconds: u64,
    child_ms: u32,
    pids: Option<String>,
}

fn main() -> ExitCode {
    let burst = match burst_from(&std::env::args().skip(1).collect::<Vec<_>>()) {
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

fn burst_from(args: &[String]) -> Result<Burst, String> {
    let usage = || "usage: forkrate PER_SECOND SECONDS CHILD_MS [--pids FILE]".to_owned();
    let number = |word: &String| word.parse().map_err(|_| usage());
    let (per_second, seconds, child_ms, pids) = match args {
        [per_second, seconds, child_ms] => (per_second, seconds, child_ms, None),
        [per_second, seconds, child_ms, flag, file] if flag == "--pids" => {
            (per_second, seconds, child_ms, Some(file.clone()))
        }
        _ => return Err(usage()),
    };

    Ok(Burst {
        per_second: number(per_second)?,
        seconds: number(seconds)?,
        child_ms: child_ms.parse().map_err(|_| usage())?,
        pids,
    })
}

fn run(burst: &Burst) -> std::io::Result<()> {
    let timebase = Timebase::read();
    let period = timebase.ticks(1_000_000_000 / burst.per_second.max(1));
    let second = timebase.ticks(1_000_000_000);
    let reap_every = timebase.ticks(10_000_000);
    let total = burst.per_second * burst.seconds;

    let start = unsafe { mach_absolute_time() };
    let mut next = start;
    let mut next_reap = start + reap_every;
    let mut second_ends = start + second;
    let mut this_second = 0_u64;
    let mut per_second_reached = Vec::new();
    let mut children = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    let mut late = 0_u64;
    let mut reaped = 0_u64;

    for _ in 0..total {
        children.push(fork_child(burst.child_ms)?);
        this_second += 1;
        next += period;

        let now = unsafe { mach_absolute_time() };
        if now >= next_reap {
            reaped += reap();
            next_reap += reap_every;
        }
        if now >= second_ends {
            per_second_reached.push(this_second);
            this_second = 0;
            second_ends += second;
        }
        if now > next {
            late += 1;
        }

        unsafe { mach_wait_until(next) };
    }
    per_second_reached.push(this_second);
    while reaped < total && unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) } > 0 {
        reaped += 1;
    }

    let elapsed = Duration::from_nanos(timebase.nanos(unsafe { mach_absolute_time() } - start));
    let reached: Vec<String> = per_second_reached.iter().map(u64::to_string).collect();
    println!("forks each second: {}", reached.join(" "));
    println!(
        "{total} forks in {:.1} s, {:.0} a second, {late} late, {reaped} reaped",
        elapsed.as_secs_f64(),
        f64::from(u32::try_from(total).unwrap_or(u32::MAX)) / elapsed.as_secs_f64()
    );

    if let Some(file) = &burst.pids {
        let mut out = std::io::BufWriter::new(std::fs::File::create(file)?);
        for pid in &children {
            writeln!(out, "{pid}")?;
        }
        out.flush()?;
    }

    Ok(())
}

fn fork_child(child_ms: u32) -> std::io::Result<i32> {
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            if child_ms > 0 {
                libc::usleep(child_ms.saturating_mul(1_000));
            }
            libc::_exit(0);
        }
    }
    if pid < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(pid)
}

fn reap() -> u64 {
    let mut reaped = 0;
    while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {
        reaped += 1;
    }
    reaped
}
