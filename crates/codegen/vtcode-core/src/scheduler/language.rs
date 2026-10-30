//! Parses reminder and task-management commands from session language.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use regex::Regex;
use std::sync::LazyLock;

use super::{SessionLanguageCommand, parse_local_datetime};

static REMIND_AT_RE: LazyLock<std::result::Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?ix)^\s*remind\s+me\s+at\s+(?P<when>.+?)\s+to\s+(?P<prompt>.+)\s*$"));

static REMIND_IN_RE: LazyLock<std::result::Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^\s*in\s+
        (?P<count>\d+)
        \s*
        (?P<unit>minutes|minute|hours|hour|days|day)
        \s*,?\s*
        (?P<prompt>.+)
        \s*$
    ",
    )
});

/// Recognizes task-list, cancellation, and reminder commands relative to `now`.
///
/// Unrecognized input and out-of-range relative reminders return `None`;
/// recognized reminders with invalid absolute times retain their parse errors.
pub fn parse_session_language_command(input: &str, now: DateTime<Local>) -> Option<Result<SessionLanguageCommand>> {
    parse_language_command(input, now).transpose()
}

/// Dispatches commands in priority order while preserving regex initialization errors.
fn parse_language_command(input: &str, now: DateTime<Local>) -> Result<Option<SessionLanguageCommand>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let normalized = trimmed.trim_end_matches(['?', '.', '!']).to_ascii_lowercase();
    if normalized == "what scheduled tasks do i have" {
        return Ok(Some(SessionLanguageCommand::ListTasks));
    }

    if let Some(command) = parse_remind_at(trimmed, now)? {
        return Ok(Some(command));
    }
    if let Some(command) = parse_remind_in(trimmed, now)? {
        return Ok(Some(command));
    }

    Ok(trimmed
        .strip_prefix("cancel ")
        .map(|query| SessionLanguageCommand::CancelTask { query: query.trim().to_string() }))
}

/// Resolves a recognized absolute reminder using the shared local-time parser.
fn parse_remind_at(input: &str, now: DateTime<Local>) -> Result<Option<SessionLanguageCommand>> {
    let regex = REMIND_AT_RE.as_ref().context("Failed to compile the remind-at regex")?;
    let Some(captures) = regex.captures(input) else {
        return Ok(None);
    };
    let when = captures.name("when").map(|value| value.as_str()).unwrap_or_default();
    let prompt = captures
        .name("prompt")
        .map(|value| value.as_str().trim().to_string())
        .unwrap_or_default();
    parse_local_datetime(when, now).map(|run_at| Some(SessionLanguageCommand::CreateOneShotPrompt { prompt, run_at }))
}

/// Recognizes relative reminders only when both duration and deadline fit Chrono's ranges.
fn parse_remind_in(input: &str, now: DateTime<Local>) -> Result<Option<SessionLanguageCommand>> {
    let regex = REMIND_IN_RE.as_ref().context("Failed to compile the remind-in regex")?;
    Ok(regex.captures(input).and_then(|captures| {
        let count = captures["count"].parse::<i64>().ok()?;
        let prompt = captures["prompt"].trim().to_string();
        let delta = match captures["unit"].to_ascii_lowercase().as_str() {
            "minute" | "minutes" => ChronoDuration::try_minutes(count),
            "hour" | "hours" => ChronoDuration::try_hours(count),
            "day" | "days" => ChronoDuration::try_days(count),
            _ => return None,
        }?;
        let run_at = now.checked_add_signed(delta)?.with_timezone(&Utc);
        Some(SessionLanguageCommand::CreateOneShotPrompt { prompt, run_at })
    }))
}
