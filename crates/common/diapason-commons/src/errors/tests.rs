//! Extracted regression tests; production source remains byte-identical.

use anyhow::Error;

use super::*;

#[test]
fn formatter_uses_display() {
    let formatter = DisplayErrorFormatter;
    let error = Error::msg("test error");
    assert_eq!(formatter.format_error(&error), "test error");
}

#[test]
fn noop_reporter_drops_errors() {
    let reporter = NoopErrorReporter;
    let error = Error::msg("test");
    assert!(reporter.capture(&error).is_ok());
    assert!(reporter.capture_message("message").is_ok());
}

#[test]
fn multi_errors_new_is_empty() {
    let errors: MultiErrors<String> = MultiErrors::new();
    assert!(errors.is_empty());
    assert_eq!(errors.len(), 0);
}

#[test]
fn multi_errors_push_and_len() {
    let mut errors = MultiErrors::new();
    errors.push("error 1".to_string());
    errors.push("error 2".to_string());
    assert!(!errors.is_empty());
    assert_eq!(errors.len(), 2);
}

#[test]
fn multi_errors_collect_result_ok() {
    let mut errors: MultiErrors<String> = MultiErrors::new();
    let value: i32 = errors.collect_result(Ok::<_, String>(42)).unwrap_or(0);
    assert_eq!(value, 42);
    assert!(errors.is_empty());
}

#[test]
fn multi_errors_collect_result_err() {
    let mut errors: MultiErrors<String> = MultiErrors::new();
    let value: i32 = errors.collect_result(Err::<i32, String>("bad".to_string())).unwrap_or(0);
    assert_eq!(value, 0);
    assert_eq!(errors.len(), 1);
}

#[test]
fn multi_errors_ok_succeeds_when_empty() {
    let errors: MultiErrors<String> = MultiErrors::new();
    assert!(errors.ok().is_ok());
}

#[test]
fn multi_errors_ok_fails_when_not_empty() {
    let mut errors = MultiErrors::new();
    errors.push("error".to_string());
    assert!(errors.ok().is_err());
}

#[test]
fn multi_errors_display_empty() {
    let errors: MultiErrors<String> = MultiErrors::new();
    assert_eq!(errors.to_string(), "no errors");
}

#[test]
fn multi_errors_display_single() {
    let mut errors = MultiErrors::new();
    errors.push("something failed".to_string());
    assert_eq!(errors.to_string(), "something failed");
}

#[test]
fn multi_errors_display_multiple() {
    let mut errors = MultiErrors::new();
    errors.push("first issue".to_string());
    errors.push("second issue".to_string());
    let display = errors.to_string();
    assert!(display.contains("1. first issue"));
    assert!(display.contains("2. second issue"));
}

#[test]
fn multi_errors_extend() {
    let mut errors = MultiErrors::new();
    errors.extend(vec!["a".to_string(), "b".to_string()]);
    assert_eq!(errors.len(), 2);
}

#[test]
fn multi_errors_into_inner() {
    let mut errors = MultiErrors::new();
    errors.push("test".to_string());
    let inner: Vec<String> = errors.into_inner();
    assert_eq!(inner.len(), 1);
}

#[test]
fn multi_errors_from_vec() {
    let errors: MultiErrors<String> = MultiErrors::from(vec!["a".to_string()]);
    assert_eq!(errors.len(), 1);
}

#[test]
fn multi_errors_into_iterator() {
    let mut errors = MultiErrors::new();
    errors.push("a".to_string());
    errors.push("b".to_string());
    let collected: Vec<String> = errors.into_iter().collect();
    assert_eq!(collected, vec!["a", "b"]);
}

#[test]
fn multi_errors_slice_access() {
    let mut errors = MultiErrors::new();
    errors.push("err".to_string());
    assert_eq!(errors.as_slice(), &["err".to_string()]);
}

#[test]
fn multi_errors_to_anyhow() {
    let mut errors = MultiErrors::new();
    errors.push("something broke".to_string());
    let err = errors.to_anyhow();
    assert!(err.to_string().contains("something broke"));
}
