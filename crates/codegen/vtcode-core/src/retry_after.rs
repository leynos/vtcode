use std::time::{Duration, SystemTime};

use vtcode_commons::llm::LLMErrorMetadata;

/// Longest provider-supplied delta accepted for one retry wait.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

fn parse_retry_after_header_at(raw: &str, now: SystemTime) -> Option<Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(delay) = parse_delta_seconds(raw) {
        return Some(delay);
    }

    let retry_at = httpdate::parse_http_date(raw).ok()?;
    Some(retry_at.duration_since(now).unwrap_or(Duration::ZERO))
}

fn parse_delta_seconds(raw: &str) -> Option<Duration> {
    if !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(raw.parse::<u64>().map_or(MAX_RETRY_AFTER, saturating_seconds));
    }

    let (whole, fraction) = raw.split_once('.')?;
    let decimal = DecimalSeconds { whole, fraction };
    if !decimal.is_valid() {
        return None;
    }

    let nanoseconds = decimal.nanoseconds()?;
    let Some((seconds, nanoseconds)) = carry_nanoseconds(decimal.seconds(), nanoseconds) else {
        return Some(MAX_RETRY_AFTER);
    };

    Some(Duration::new(seconds, nanoseconds).min(MAX_RETRY_AFTER))
}

/// A `whole.fraction` seconds pair split from a `Retry-After` delta.
struct DecimalSeconds<'a> {
    whole: &'a str,
    fraction: &'a str,
}

impl DecimalSeconds<'_> {
    /// Accepts a pair where at least one side is non-empty and both sides are
    /// decimal digits.
    fn is_valid(&self) -> bool {
        if self.whole.is_empty() && self.fraction.is_empty() {
            return false;
        }
        self.whole.bytes().all(|byte| byte.is_ascii_digit()) && self.fraction.bytes().all(|byte| byte.is_ascii_digit())
    }

    /// Whole seconds, saturating when the integer part exceeds `u64`.
    ///
    /// Saturated values are clamped by [`MAX_RETRY_AFTER`] before they become
    /// a wait, so they never escape as a real delay.
    fn seconds(&self) -> u64 {
        if self.whole.is_empty() {
            return 0;
        }
        self.whole.parse().unwrap_or(u64::MAX)
    }

    /// Converts the fractional seconds to nanoseconds, rounding up any digits
    /// beyond nanosecond precision so a sub-nanosecond wait is never dropped.
    fn nanoseconds(&self) -> Option<u32> {
        let digits = self.fraction.get(..self.fraction.len().min(9))?;
        let scale = 10_u32.pow(u32::try_from(9 - digits.len()).ok()?);
        let base = if digits.is_empty() {
            0
        } else {
            digits.parse::<u32>().ok()?
        };
        let rounds_up = self
            .fraction
            .as_bytes()
            .get(digits.len()..)
            .is_some_and(|tail| tail.iter().any(|digit| *digit != b'0'));
        base.checked_mul(scale)?.checked_add(u32::from(rounds_up))
    }
}

fn saturating_seconds(seconds: u64) -> Duration {
    Duration::from_secs(seconds).min(MAX_RETRY_AFTER)
}

/// Folds a full second of nanoseconds into the seconds field.
///
/// `None` reports a seconds overflow, which callers saturate.
fn carry_nanoseconds(seconds: u64, nanoseconds: u32) -> Option<(u64, u32)> {
    if nanoseconds < 1_000_000_000 {
        return Some((seconds, nanoseconds));
    }
    Some((seconds.checked_add(1)?, 0))
}

pub(crate) fn retry_after_is_http_date(metadata: &LLMErrorMetadata) -> bool {
    metadata.retry_after.as_deref().is_some_and(|raw| {
        let raw = raw.trim();
        parse_delta_seconds(raw).is_none() && httpdate::parse_http_date(raw).is_ok()
    })
}

pub(crate) fn retry_after_from_llm_metadata_at(metadata: &LLMErrorMetadata, now: SystemTime) -> Option<Duration> {
    let retry_after = metadata
        .retry_after
        .as_deref()
        .and_then(|raw| parse_retry_after_header_at(raw, now));
    let reset_after = metadata
        .rate_limit
        .as_ref()
        .and_then(|rate_limit| rate_limit.reset_after_millis)
        .map(Duration::from_millis);
    retry_after.max(reset_after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode_commons::llm::RateLimitMetadata;

    #[test]
    fn retry_after_header_accepts_integer_seconds() {
        assert_eq!(parse_retry_after_header_at(" 7 ", SystemTime::UNIX_EPOCH), Some(Duration::from_secs(7)));
    }

    #[test]
    fn retry_after_header_accepts_fractional_seconds() {
        assert_eq!(parse_retry_after_header_at("0.5", SystemTime::UNIX_EPOCH), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after_header_at("0.0000000001", SystemTime::UNIX_EPOCH), Some(Duration::from_nanos(1)));
    }

    #[test]
    fn retry_after_header_accepts_http_date_relative_to_injected_time() {
        let retry_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_480);
        let now = retry_at - Duration::from_millis(1_500);

        assert_eq!(
            parse_retry_after_header_at("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(Duration::from_millis(1_500))
        );
    }

    #[test]
    fn retry_after_delta_seconds_cap_huge_values() {
        assert_eq!(parse_retry_after_header_at("18446744073709551616", SystemTime::UNIX_EPOCH), Some(MAX_RETRY_AFTER));
        assert_eq!(
            parse_retry_after_header_at("18446744073709551615.9999999999", SystemTime::UNIX_EPOCH),
            Some(MAX_RETRY_AFTER)
        );
    }

    #[test]
    fn retry_after_http_date_in_the_past_means_no_additional_wait() {
        let retry_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_480);
        let now = retry_at + Duration::from_secs(1);

        assert_eq!(parse_retry_after_header_at("Wed, 21 Oct 2015 07:28:00 GMT", now), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_http_date_is_identified_without_treating_delta_as_absolute() {
        let date_metadata = LLMErrorMetadata::new(
            "Together",
            Some(429),
            None,
            None,
            None,
            Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
            None,
        );
        let delta_metadata =
            LLMErrorMetadata::new("Together", Some(429), None, None, None, Some("7".to_string()), None);

        assert!(retry_after_is_http_date(&date_metadata));
        assert!(!retry_after_is_http_date(&delta_metadata));
    }

    #[test]
    fn retry_after_header_rejects_empty_or_invalid_values() {
        for raw in ["", "soon", "-1", "inf", "1e20", "Wed, 99 Oct 2015 07:28:00 GMT"] {
            assert_eq!(parse_retry_after_header_at(raw, SystemTime::UNIX_EPOCH), None);
        }
    }

    /// Metadata carrying a raw `Retry-After` header and a reset floor.
    fn metadata_with(retry_after: &str, reset_after_millis: u64) -> Box<LLMErrorMetadata> {
        LLMErrorMetadata::new("Together", Some(429), None, None, None, Some(retry_after.to_string()), None)
            .with_rate_limit(Some(RateLimitMetadata {
                reset_after_millis: Some(reset_after_millis),
                ..RateLimitMetadata::default()
            }))
    }

    #[test]
    fn metadata_uses_larger_of_retry_after_and_rate_limit_reset() {
        let metadata = metadata_with("0.5", 750);

        assert_eq!(
            retry_after_from_llm_metadata_at(&metadata, SystemTime::UNIX_EPOCH),
            Some(Duration::from_millis(750))
        );
    }

    #[test]
    fn metadata_uses_rate_limit_reset_when_retry_after_is_malformed() {
        let metadata = metadata_with("soon", 250);

        assert_eq!(
            retry_after_from_llm_metadata_at(&metadata, SystemTime::UNIX_EPOCH),
            Some(Duration::from_millis(250))
        );
    }
}
