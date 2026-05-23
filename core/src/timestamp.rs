use std::time::{SystemTime, UNIX_EPOCH};

/// Dual-clock timestamp used throughout the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Timestamp {
    /// Monotonic nanoseconds for local ordering and interval calculation.
    pub mono_ns: u64,
    /// UTC-wallclock nanoseconds for cross-node correlation and OWD estimation.
    pub utc_ns: u64,
}

impl Timestamp {
    /// Sample both clocks as atomically as practical.
    /// `mono_ns` comes from `std::time::Instant` (via `libc::CLOCK_MONOTONIC` on Linux).
    /// `utc_ns` comes from `std::time::SystemTime` (`CLOCK_REALTIME`).
    pub fn now() -> Self {
        // We sample mono first, then utc immediately after.
        // The asymmetry is bounded by a few nanoseconds on modern kernels.
        let mono_ns = instant_mono_ns();
        let utc_ns = system_utc_ns();
        Self { mono_ns, utc_ns }
    }
}

fn instant_mono_ns() -> u64 {
    // Instant doesn't expose nanoseconds directly, but on Linux it is
    // CLOCK_MONOTONIC. We use libc directly for a raw nanosecond value.
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64)
}

fn system_utc_ns() -> u64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before Unix epoch");
    dur.as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_non_decreasing() {
        let a = instant_mono_ns();
        let b = instant_mono_ns();
        assert!(b >= a, "monotonic clock went backwards: {} -> {}", a, b);
    }

    #[test]
    fn utc_reasonable_range() {
        let utc = system_utc_ns();
        // Between 2024-01-01 and 2035-01-01 in nanoseconds since epoch
        let min = 1_704_067_200_000_000_000u64;
        let max = 2_051_222_400_000_000_000u64;
        assert!(
            utc >= min && utc <= max,
            "utc_ns out of expected range: {}",
            utc
        );
    }
}
