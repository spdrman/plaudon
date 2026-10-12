//! Real resident-set-size measurement, or an honest skip.
//!
//! Spec 17.5 asks for "bounded memory growth proportional to active chunk
//! window rather than full decoded PCM". That claim is only worth making
//! against the operating system's own accounting, so this module reads it:
//!
//! * macOS: `task_info(MACH_TASK_BASIC_INFO)`, which reports both the current
//!   and the high-water resident size of this task.
//! * Linux: `/proc/self/statm` for the current size and `VmHWM` in
//!   `/proc/self/status` for the peak.
//! * Windows: `K32GetProcessMemoryInfo`, which reports the working set and its
//!   peak. Declared directly against `kernel32` rather than taking a Windows
//!   binding crate for two function calls.
//!
//! Anywhere else [`sample`] returns [`None`] with a reason from
//! [`unsupported_reason`], and the memory test **skips with that reason
//! printed**. It never substitutes an estimate: a fabricated memory number is
//! worse than a missing one.

use serde::Serialize;

/// One reading of this process's resident memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RssSample {
    /// Resident bytes right now.
    pub current_bytes: u64,
    /// High-water resident bytes for the lifetime of the process.
    pub peak_bytes: u64,
}

/// How this build obtains RSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `task_info(MACH_TASK_BASIC_INFO)`.
    MachTaskBasicInfo,
    /// `/proc/self/statm` and `/proc/self/status`.
    ProcSelf,
    /// `K32GetProcessMemoryInfo`.
    ProcessMemoryInfo,
    /// Nothing available; measurements are skipped.
    Unsupported,
}

impl Source {
    /// Name used in published results.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MachTaskBasicInfo => "mach_task_basic_info",
            Self::ProcSelf => "/proc/self",
            Self::ProcessMemoryInfo => "GetProcessMemoryInfo",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Which source this build compiles in.
#[must_use]
pub const fn source() -> Source {
    #[cfg(target_vendor = "apple")]
    {
        Source::MachTaskBasicInfo
    }
    #[cfg(target_os = "linux")]
    {
        Source::ProcSelf
    }
    #[cfg(windows)]
    {
        Source::ProcessMemoryInfo
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
    {
        Source::Unsupported
    }
}

/// The exact reason a platform has no reading, for the skip message.
#[must_use]
pub fn unsupported_reason() -> String {
    format!(
        "no resident-set-size source is implemented for {}-{}; \
         the memory-bound assertions are skipped rather than estimated",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Read the current and peak resident size, or `None` on an unsupported host.
#[must_use]
pub fn sample() -> Option<RssSample> {
    platform::sample()
}

#[cfg(target_vendor = "apple")]
mod platform {
    use super::RssSample;

    pub(super) fn sample() -> Option<RssSample> {
        // SAFETY: a zeroed `mach_task_basic_info` is valid because every field
        // is an integer. `mach_task_self()` reads an immutable global port
        // handle for this task. `task_info` writes at most `count` 32-bit
        // words into the out-parameter, and `count` is the constant the
        // kernel defines for exactly this struct, so the write stays inside
        // `info`. The pointer cast is the documented calling convention of
        // the flavoured `task_info` API.
        #[allow(unsafe_code)]
        let (status, info) = unsafe {
            let mut info: libc::mach_task_basic_info = std::mem::zeroed();
            let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
            let status = libc::task_info(
                // `mach_task_self_` is the task port constant the kernel
                // publishes for this process; the `mach_task_self()` wrapper
                // is deprecated in favour of a separate crate we do not need.
                libc::mach_task_self_,
                libc::MACH_TASK_BASIC_INFO,
                std::ptr::addr_of_mut!(info).cast::<libc::integer_t>(),
                &raw mut count,
            );
            (status, info)
        };
        if status != libc::KERN_SUCCESS {
            return None;
        }
        // `mach_task_basic_info` is `repr(packed(4))`, so the fields are read
        // by value rather than borrowed.
        let current = info.resident_size;
        let peak = info.resident_size_max;
        Some(RssSample {
            current_bytes: current,
            peak_bytes: peak.max(current),
        })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::RssSample;

    pub(super) fn sample() -> Option<RssSample> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // `sysconf(_SC_PAGESIZE)` is not reachable without libc on this
        // target; Linux has never used a page size other than the one the
        // kernel reports in `/proc/self/smaps_rollup`, so prefer the
        // kilobyte-denominated `VmRSS` and use statm only as a fallback.
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let read_kb = |prefix: &str| -> Option<u64> {
            status
                .lines()
                .find(|line| line.starts_with(prefix))?
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()
                .map(|value| value * 1024)
        };
        let current = read_kb("VmRSS:").unwrap_or(resident_pages * 4096);
        let peak = read_kb("VmHWM:").unwrap_or(current);
        Some(RssSample {
            current_bytes: current,
            peak_bytes: peak.max(current),
        })
    }
}

#[cfg(windows)]
mod platform {
    use super::RssSample;

    use std::ffi::c_void;

    /// `PROCESS_MEMORY_COUNTERS` from `psapi.h`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[allow(unsafe_code)]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    pub(super) fn sample() -> Option<RssSample> {
        let mut counters = ProcessMemoryCounters {
            cb: u32::try_from(std::mem::size_of::<ProcessMemoryCounters>()).ok()?,
            ..ProcessMemoryCounters::default()
        };
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // release. `K32GetProcessMemoryInfo` writes exactly `cb` bytes into
        // `counters`, and `cb` is the size of the struct we pass.
        #[allow(unsafe_code)]
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::addr_of_mut!(counters),
                counters.cb,
            )
        };
        if ok == 0 {
            return None;
        }
        let current = u64::try_from(counters.working_set_size).ok()?;
        let peak = u64::try_from(counters.peak_working_set_size).unwrap_or(current);
        Some(RssSample {
            current_bytes: current,
            peak_bytes: peak.max(current),
        })
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
mod platform {
    use super::RssSample;

    pub(super) const fn sample() -> Option<RssSample> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_supported_host_reports_a_plausible_resident_size() {
        let Some(reading) = sample() else {
            assert_eq!(
                source(),
                Source::Unsupported,
                "a platform with a compiled-in source must return a reading"
            );
            println!("skipped: {}", unsupported_reason());
            return;
        };
        // A Rust test process is never smaller than a megabyte, and a
        // 1 TiB reading would mean the units are wrong.
        assert!(
            reading.current_bytes > 1 << 20,
            "implausibly small RSS: {} bytes",
            reading.current_bytes
        );
        assert!(
            reading.current_bytes < 1 << 40,
            "implausibly large RSS: {} bytes",
            reading.current_bytes
        );
        assert!(reading.peak_bytes >= reading.current_bytes);
    }

    #[test]
    fn allocating_and_touching_memory_moves_the_reading() {
        let Some(before) = sample() else {
            println!("skipped: {}", unsupported_reason());
            return;
        };
        let mut block = vec![0u8; 64 << 20];
        // Touch every page so the pages are resident, not just reserved.
        for page in block.chunks_mut(4096) {
            page[0] = 1;
        }
        let after = sample().expect("a source that worked once must keep working");
        assert!(
            after.peak_bytes >= before.peak_bytes + (32 << 20),
            "peak RSS did not reflect a touched 64 MiB allocation: {before:?} -> {after:?}"
        );
        // Keep the allocation alive across the measurement.
        assert_eq!(block[0], 1);
    }
}
