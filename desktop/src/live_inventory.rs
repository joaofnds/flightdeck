use std::ffi::CStr;
use std::fmt::Display;
use std::mem;
use std::ptr;
use std::sync::OnceLock;

#[must_use]
pub fn rows() -> Vec<(String, String)> {
    vec![
        row("Model identifier", string(c"hw.product")),
        row("Chip", string(c"machdep.cpu.brand_string")),
        row("Architecture", string(c"hw.machine")),
        row(
            "Physical CPU cores · maximum this boot",
            positive_count(c"hw.physicalcpu_max"),
        ),
        row(
            "Logical CPUs · maximum this boot",
            positive_count(c"hw.logicalcpu_max"),
        ),
        row("Installed memory", memory()),
        row("macOS version", string(c"kern.osproductversion")),
        row("macOS build", string(c"kern.osversion")),
        row("Darwin kernel", string(c"kern.osrelease")),
        row("VM page size · bytes", positive_count(c"hw.pagesize")),
        row("Boot time", boot()),
    ]
}

/// One performance level of this Mac's CPUs, named as macOS names it.
pub struct PerformanceLevel {
    pub name: String,
    pub cores: String,
    pub logical_cpus: u32,
}

/// This Mac's two performance levels, fastest first, or none on a Mac without two levels
/// or whose levels could not be read.
pub fn performance_levels() -> Option<&'static [PerformanceLevel; 2]> {
    static LEVELS: OnceLock<Option<[PerformanceLevel; 2]>> = OnceLock::new();
    LEVELS
        .get_or_init(|| match performance_levels_read() {
            Ok(levels) => levels,
            Err(error) => {
                eprintln!("flightdeck performance levels unreadable: {error}");
                None
            }
        })
        .as_ref()
}

/// A logical CPU named by its performance level as the prototype names it, such as
/// "super 12". macOS numbers the slower level's CPUs first. A CPU past both levels, or a Mac
/// whose levels could not be read, keeps its plain index.
#[must_use]
pub fn logical_cpu_name(cpu: u32) -> String {
    logical_cpu_level_name(performance_levels(), cpu)
}

fn logical_cpu_level_name(levels: Option<&[PerformanceLevel; 2]>, cpu: u32) -> String {
    let Some([fastest, slower]) = levels else {
        return format!("CPU {cpu}");
    };
    if cpu < slower.logical_cpus {
        return format!("{} {cpu}", slower.name.to_lowercase());
    }
    if cpu - slower.logical_cpus < fastest.logical_cpus {
        return format!("{} {cpu}", fastest.name.to_lowercase());
    }
    format!("CPU {cpu}")
}

fn performance_levels_read() -> Result<Option<[PerformanceLevel; 2]>, String> {
    let Some(levels) =
        collector::machine::performance_levels().map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let [fastest, slower] = levels.logical_cpus;
    Ok(Some([
        performance_levels_level(c"hw.perflevel0.name", fastest)?,
        performance_levels_level(c"hw.perflevel1.name", slower)?,
    ]))
}

fn performance_levels_level(key: &CStr, logical_cpus: u32) -> Result<PerformanceLevel, String> {
    let name = string(key)?;
    Ok(PerformanceLevel {
        cores: format!("{name} cores"),
        name,
        logical_cpus,
    })
}

fn row(label: &str, reading: Result<impl Display, String>) -> (String, String) {
    let value = match reading {
        Ok(value) => value.to_string(),
        Err(error) => format!("Unavailable · {error}"),
    };
    (label.into(), value)
}

fn string(key: &CStr) -> Result<String, String> {
    let mut bytes = [0_u8; 512];
    let mut count = bytes.len();
    read_kernel(key, bytes.as_mut_ptr().cast(), &mut count)?;
    if count <= 1 {
        return Err("The kernel returned an empty string".into());
    }
    if count > bytes.len() {
        return Err(format!("Invalid string byte count: {count}"));
    }
    if bytes[count - 1] != 0 {
        return Err("The string has no final null terminator".into());
    }
    let content = &bytes[..count - 1];
    if content.contains(&0) {
        return Err("The string contains an embedded null terminator".into());
    }
    String::from_utf8(content.to_vec()).map_err(|error| format!("Invalid UTF-8: {error}"))
}

fn positive_count(key: &CStr) -> Result<i32, String> {
    let mut value = 0_i32;
    read_value(key, &mut value)?;
    if value <= 0 {
        return Err("Expected a positive value".into());
    }
    Ok(value)
}

fn memory() -> Result<String, String> {
    let mut bytes = 0_u64;
    read_value(c"hw.memsize", &mut bytes)?;
    if bytes == 0 {
        return Err("Expected a positive value".into());
    }
    let gibibytes = bytes / 1_073_741_824;
    let hundredths = (bytes % 1_073_741_824) * 100 / 1_073_741_824;
    Ok(format!("{gibibytes}.{hundredths:02} GiB · {bytes} bytes"))
}

fn boot() -> Result<String, String> {
    let mut value = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    read_value(c"kern.boottime", &mut value)?;
    if value.tv_sec <= 0 {
        return Err("Invalid boot timeval: nonpositive seconds".into());
    }
    if value.tv_usec < 0 {
        return Err("Invalid boot timeval: negative microseconds".into());
    }
    if value.tv_usec >= 1_000_000 {
        return Err("Invalid boot timeval: microseconds exceed one second".into());
    }
    boot_format(&value)
}

fn boot_format(value: &libc::timeval) -> Result<String, String> {
    // localtime_r owns no resource; all output storage stays on this stack.
    let mut calendar = unsafe { mem::zeroed::<libc::tm>() };
    if unsafe { libc::localtime_r(&raw const value.tv_sec, &raw mut calendar) }.is_null() {
        return Err("The boot time could not be converted to local time".into());
    }
    let mut bytes = [0_u8; 64];
    let count = unsafe {
        libc::strftime(
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            c"%Y-%m-%d %H:%M:%S %z".as_ptr(),
            &raw const calendar,
        )
    };
    if count == 0 {
        return Err("The boot time could not be formatted".into());
    }
    if count >= bytes.len() {
        return Err("The formatted boot time exceeded its buffer".into());
    }
    String::from_utf8(bytes[..count].to_vec()).map_err(|error| format!("Invalid UTF-8: {error}"))
}

fn read_value<T>(key: &CStr, value: &mut T) -> Result<(), String> {
    let mut count = mem::size_of::<T>();
    read_kernel(key, ptr::from_mut(value).cast(), &mut count)?;
    if count != mem::size_of::<T>() {
        return Err(format!(
            "Invalid byte count: expected {}, received {count}",
            mem::size_of::<T>()
        ));
    }
    Ok(())
}

fn read_kernel(key: &CStr, buffer: *mut libc::c_void, count: &mut usize) -> Result<(), String> {
    // Each caller supplies initialized storage and its exact byte capacity.
    let status = unsafe { libc::sysctlbyname(key.as_ptr(), buffer, count, ptr::null_mut(), 0) };
    if status != 0 {
        return Err(format!(
            "{}: {}",
            key.to_string_lossy(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_cpu_level_name_numbers_the_slower_level_first() {
        // Goal: CPUs read as the prototype labels them, slower level first, then the fastest.
        // Method: twelve performance and six super CPUs, with an index past both and no levels.
        let level = |name: &str, logical_cpus| PerformanceLevel {
            name: name.into(),
            cores: String::new(),
            logical_cpus,
        };
        let levels = [level("Super", 6), level("Performance", 12)];

        let names = [0, 11, 12, 17, 18].map(|cpu| logical_cpu_level_name(Some(&levels), cpu));

        assert_eq!(
            names,
            [
                "performance 0",
                "performance 11",
                "super 12",
                "super 17",
                "CPU 18"
            ]
        );
        assert_eq!(logical_cpu_level_name(None, 3), "CPU 3");
    }
}
