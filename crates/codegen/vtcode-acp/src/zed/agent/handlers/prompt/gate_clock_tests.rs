//! Deterministic coverage for the injectable prompt-gate wait clock.
//!
//! The prompt runner takes a clock so gate-wait telemetry does not force tests
//! to sleep; these cases pin its measurement rules.

use super::*;

#[test]
fn gate_wait_ms_reports_elapsed_milliseconds() {
    let started = Instant::now();
    let finished = started + Duration::from_millis(2_500);

    assert_eq!(gate_wait_ms(started, finished), 2_500);
}

#[test]
fn gate_wait_ms_is_zero_for_the_same_sample() {
    let sample = Instant::now();

    assert_eq!(gate_wait_ms(sample, sample), 0);
}

#[test]
fn gate_wait_ms_saturates_when_the_clock_goes_backwards() {
    let started = Instant::now();
    let earlier = started.checked_sub(Duration::from_secs(1)).unwrap_or(started);

    assert_eq!(gate_wait_ms(started, earlier), 0);
}
