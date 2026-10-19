use std::ffi::CStr;
use std::fmt::Display;
use std::mem;
use std::ptr;
use std::sync::OnceLock;

/// This Mac's identity and the sizes the Computer page's model card shows.
#[must_use]
pub fn rows() -> Vec<(String, String)> {
    vec![
        row("Model identifier", string(c"hw.product")),
        row("Chip", string(c"machdep.cpu.brand_string")),
        row("Architecture", string(c"hw.machine")),
        row("CPU cores", positive_count(c"hw.physicalcpu_max")),
        row("GPU cores", gpu_cores()),
        row("Memory", memory()),
    ]
}

/// One performance level of this Mac's CPUs, named as macOS names it.
pub struct PerformanceLevel {
    pub name: String,
    pub cores: String,
    /// The same, in lower case, as a chart's legend names a cluster.
    pub cluster_cores: String,
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
        cluster_cores: format!("{} cores", name.to_lowercase()),
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
    Ok(memory_format(bytes))
}

/// Whole gibibytes as the prototype writes them, such as "48 GiB", and any other size to
/// the hundredth below it.
fn memory_format(bytes: u64) -> String {
    let gibibytes = bytes / 1_073_741_824;
    let remainder = bytes % 1_073_741_824;
    if remainder == 0 {
        return format!("{gibibytes} GiB");
    }
    // The hundredths floor, so a size never reads larger than it is.
    let hundredths = remainder * 100 / 1_073_741_824;
    format!("{gibibytes}.{hundredths:02} GiB")
}

fn gpu_cores() -> Result<u64, String> {
    match collector::machine::gpu_cores() {
        Ok(Some(cores)) => Ok(cores),
        Ok(None) => Err("The GPU driver reports no core count".into()),
        Err(error) => Err(error.to_string()),
    }
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
    fn memory_format_writes_whole_gibibytes_without_decimals() {
        // Goal: memory reads as the prototype writes it, with decimals only where they count.
        // Method: format 48 GiB exactly, and 47.5 GiB less one byte, which floors.
        let gibibyte = 1_073_741_824_u64;

        let sizes = [48 * gibibyte, 47 * gibibyte + gibibyte / 2 - 1].map(memory_format);

        assert_eq!(sizes, ["48 GiB", "47.49 GiB"]);
    }

    #[test]
    fn logical_cpu_level_name_numbers_the_slower_level_first() {
        // Goal: CPUs read as the prototype labels them, slower level first, then the fastest.
        // Method: twelve performance and six super CPUs, with an index past both and no levels.
        let level = |name: &str, logical_cpus| PerformanceLevel {
            name: name.into(),
            cores: String::new(),
            cluster_cores: String::new(),
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
