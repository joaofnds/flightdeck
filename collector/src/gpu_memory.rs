//! The unified memory the GPU driver reports in its performance statistics: the memory GPU
//! work uses now and the memory the driver has allocated for it.

use std::io;

use crate::registry::{Entry, Key};

/// The GPU driver's entry and the property names a reading looks up, made once, so each
/// reading only fetches its statistics.
pub struct GpuMemory {
    accelerator: Entry,
    statistics: Key,
    names: [Key; 2],
}

/// One reading of the GPU driver's memory, in bytes, each none where the driver left it out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMemoryReading {
    pub in_use_bytes: Option<u64>,
    pub allocated_bytes: Option<u64>,
}

impl GpuMemory {
    /// The GPU driver's entry, or an error where the registry has none.
    pub fn open() -> io::Result<Self> {
        Ok(Self {
            accelerator: Entry::of_class(c"AGXAccelerator", "GPU accelerator")?,
            statistics: Key::new("PerformanceStatistics")?,
            names: [
                Key::new("In use system memory")?,
                Key::new("Alloc system memory")?,
            ],
        })
    }

    /// The driver's memory now, or none where it publishes no performance statistics. Each
    /// reading allocates the statistics dictionary, which the I/O Registry gives only as a
    /// CoreFoundation property, so a caller reads it at most once a second.
    #[must_use]
    pub fn read(&self) -> Option<GpuMemoryReading> {
        let [in_use_bytes, allocated_bytes] = self
            .accelerator
            .dictionary_counts(&self.statistics, &self.names)?;
        Some(GpuMemoryReading {
            in_use_bytes,
            allocated_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_gpu_memory_of_this_mac() {
        // Goal: the GPU driver of an Apple silicon Mac reports both quantities, and the memory
        // in use fits within what the driver allocated.
        // Method: read this Mac's driver statistics once.
        let memory = GpuMemory::open().unwrap();

        let reading = memory.read().unwrap();

        let in_use = reading.in_use_bytes.unwrap();
        let allocated = reading.allocated_bytes.unwrap();
        assert!(in_use > 0);
        assert!(in_use <= allocated);
    }
}
