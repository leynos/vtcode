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
        return Some(
            raw.parse::<u64>()
                .map_or(MAX_RETRY_AFTER, Duration::from_secs)
                .min(MAX_RETRY_AFTER),
        );
    }

    let (whole_seconds, fractional_seconds) = raw.split_once('.')?;
    if (whole_seconds.is_empty() && fractional_seconds.is_empty())
        || !whole_seconds.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional_seconds.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let mut seconds = if whole_seconds.is_empty() {
        0
    } else {
        match whole_seconds.parse::<u64>() {
            Ok(seconds) => seconds,
            Err(_) => return Some(MAX_RETRY_AFTER),
        }
    };
    let nanos_digits = fractional_seconds.get(..fractional_seconds.len().min(9))?;
    let mut nanoseconds = if nanos_digits.is_empty() {
        0
    } else {
        nanos_digits.parse::<u32>().ok()?
    };
    for _ in nanos_digits.len()..9 {
        nanoseconds = nanoseconds.checked_mul(10)?;
    }

    if fractional_seconds.get(nanos_digits.len()..)?.bytes().any(|byte| byte != b'0') {
        nanoseconds = nanoseconds.checked_add(1)?;
    }
    if nanoseconds == 1_000_000_000 {
        seconds = match seconds.checked_add(1) {
            Some(seconds) => seconds,
            None => return Some(MAX_RETRY_AFTER),
        };
        nanoseconds = 0;
    }

    Some(Duration::new(seconds, nanoseconds).min(MAX_RETRY_AFTER))
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

    #[test]
    fn metadata_uses_larger_of_retry_after_and_rate_limit_reset() {
        let metadata = LLMErrorMetadata::new("Together", Some(429), None, None, None, Some("0.5".to_string()), None)
            .with_rate_limit(Some(RateLimitMetadata {
                reset_after_millis: Some(750),
                ..RateLimitMetadata::default()
            }));

        assert_eq!(
            retry_after_from_llm_metadata_at(&metadata, SystemTime::UNIX_EPOCH),
            Some(Duration::from_millis(750))
        );
    }

    #[test]
    fn metadata_uses_rate_limit_reset_when_retry_after_is_malformed() {
        let metadata = LLMErrorMetadata::new("Together", Some(429), None, None, None, Some("soon".to_string()), None)
            .with_rate_limit(Some(RateLimitMetadata {
                reset_after_millis: Some(250),
                ..RateLimitMetadata::default()
            }));

        assert_eq!(
            retry_after_from_llm_metadata_at(&metadata, SystemTime::UNIX_EPOCH),
            Some(Duration::from_millis(250))
        );
    }
}
