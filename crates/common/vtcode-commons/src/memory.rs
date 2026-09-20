//! Resident Set Size (RSS) sampling for memory diagnostics.
//!
//! Used by the allocator benchmark (`vtcode bench-allocator`) to measure whether
//! the global allocator returns memory to the OS after bursty/sparse workloads.
//! Unlike `performance_profiler::get_memory_usage_mb` (Linux `/proc` only, fake
//! fallback on macOS), this returns a real value on every supported platform.
use std::time::Duration;

use num_traits::ToPrimitive;

/// Returns the current process Resident Set Size in **megabytes**, or `None` if
/// it cannot be determined on the current platform.
#[cfg(target_os = "macos")]
#[expect(
    clippy::float_arithmetic,
    reason = "RSS bytes are converted to the existing megabyte f64 API."
)]
fn resident_set_size_mb() -> Option<f64> {
    let pid = sysinfo::get_current_pid().ok()?;
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_memory(),
    );
    let bytes = system.process(pid)?.memory().to_f64()?;
    Some(bytes / (1024.0 * 1024.0))
}

/// Returns the current process Resident Set Size in **megabytes**, or `None` if
/// it cannot be determined on the current platform.
#[cfg(target_os = "linux")]
#[expect(
    clippy::float_arithmetic,
    reason = "RSS is reported in megabytes by converting the kernel's page value."
)]
pub fn resident_set_size_mb() -> Option<f64> {
    let contents = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages = contents.split_whitespace().nth(1)?.parse::<f64>().ok()?;
    let page_size = page_size::get().to_f64()?;
    Some(pages * page_size / (1024.0 * 1024.0))
}

/// Fallback for unsupported platforms.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn resident_set_size_mb() -> Option<f64> {
    None
}

/// Sample RSS once and return the value in MB (0.0 if unavailable).
pub fn sample_rss_mb() -> f64 {
    resident_set_size_mb().unwrap_or(0.0)
}

/// Sample RSS repeatedly, returning the maximum observed value in MB.
/// Useful for capturing peak memory during a burst of activity.
pub fn sample_peak_rss_mb(duration: Duration, poll_interval: Duration) -> f64 {
    let start = std::time::Instant::now();
    let mut peak = 0.0;
    while start.elapsed() < duration {
        let v = sample_rss_mb();
        if v > peak {
            peak = v;
        }
        std::thread::sleep(poll_interval);
    }
    peak
}
