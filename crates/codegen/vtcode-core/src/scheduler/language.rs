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

pub fn parse_session_language_command(input: &str, now: DateTime<Local>) -> Option<Result<SessionLanguageCommand>> {
    parse_language_command(input, now).transpose()
}

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

fn parse_remind_in(input: &str, now: DateTime<Local>) -> Result<Option<SessionLanguageCommand>> {
    let regex = REMIND_IN_RE.as_ref().context("Failed to compile the remind-in regex")?;
    Ok(regex.captures(input).and_then(|captures| {
        let count = captures["count"].parse::<i64>().ok()?;
        let prompt = captures["prompt"].trim().to_string();
        let delta = match captures["unit"].to_ascii_lowercase().as_str() {
            "minute" | "minutes" => ChronoDuration::minutes(count),
            "hour" | "hours" => ChronoDuration::hours(count),
            "day" | "days" => ChronoDuration::days(count),
            _ => return None,
        };
        Some(SessionLanguageCommand::CreateOneShotPrompt { prompt, run_at: (now + delta).with_timezone(&Utc) })
    }))
}
