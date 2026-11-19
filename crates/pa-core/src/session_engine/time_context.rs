//! The turn clock (`time_context`): an admitted turn carries a hidden row with the
//! current UTC time and the time since the previous message, so the model can
//! tell a quick reply from a return after hours or days. The row appends after
//! the history (the cached prefix stays intact) and repeats at most once per
//! refresh interval; the history itself is the state, so a resumed session or a
//! compaction that drops the last row re-arms it without extra storage.

use pa_agent::types::{AgentMessage, CustomAgentMessage};

pub const TIME_CONTEXT_CUSTOM_TYPE: &str = "time_context";

/// Minimum spacing between two clock rows in one session.
const REFRESH_INTERVAL_MS: u64 = 10 * 60 * 1000;

/// The clock row the next admitted turn carries, or `None` while the newest
/// retained row is younger than the refresh interval.
#[must_use]
pub(crate) fn due_time_context_row(history: &[AgentMessage], now_ms: u64) -> Option<AgentMessage> {
    let last_clock_ms = history
        .iter()
        .rev()
        .find(|message| is_time_context(message))
        .map(AgentMessage::timestamp);
    if let Some(last_clock_ms) = last_clock_ms {
        let since_clock_ms = now_ms.saturating_sub(u64::try_from(last_clock_ms).unwrap_or(0));
        if since_clock_ms < REFRESH_INTERVAL_MS {
            return None;
        }
    }
    let previous_ms = history
        .iter()
        .rev()
        .map(AgentMessage::timestamp)
        .find(|&timestamp| timestamp > 0)
        .and_then(|timestamp| u64::try_from(timestamp).ok());
    let custom = pa_types::session::CustomMessage {
        custom_type: TIME_CONTEXT_CUSTOM_TYPE.to_string(),
        content: pa_types::ai::UserContent::Text(time_context_text(now_ms, previous_ms)),
        display: false,
        details: None,
        timestamp: now_ms,
        rest: serde_json::Map::default(),
    };
    Some(AgentMessage::Custom(CustomAgentMessage {
        role: "custom".to_string(),
        payload: serde_json::to_value(&custom).expect("time context row payload serializes"),
    }))
}

fn is_time_context(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::Custom(custom) => {
            custom
                .payload
                .get("customType")
                .and_then(serde_json::Value::as_str)
                == Some(TIME_CONTEXT_CUSTOM_TYPE)
        }
        AgentMessage::Standard(_) => false,
    }
}

fn time_context_text(now_ms: u64, previous_ms: Option<u64>) -> String {
    let elapsed = previous_ms.map_or_else(
        || "none (this is the first message of the session)".to_string(),
        // A backward clock step reads as zero elapsed time.
        |previous_ms| format_duration(now_ms.saturating_sub(previous_ms) / 1000),
    );
    format!(
        "[time-context]\n\nCurrent time: {} (UTC).\nElapsed since the previous message: {elapsed}.",
        format_utc(now_ms / 1000)
    )
}

fn format_duration(total_seconds: u64) -> String {
    let days = total_seconds / 86_400;
    let hours = total_seconds % 86_400 / 3600;
    let minutes = total_seconds % 3600 / 60;
    let seconds = total_seconds % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

fn format_utc(unix_seconds: u64) -> String {
    let days = unix_seconds / 86_400;
    let seconds_of_day = unix_seconds % 86_400;
    // Civil-from-days algorithm (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        seconds_of_day / 3600,
        seconds_of_day % 3600 / 60,
        seconds_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-19T14:32:05Z
    const NOW_MS: u64 = 1_795_098_725_000;

    fn user_at(timestamp_ms: u64) -> AgentMessage {
        AgentMessage::Standard(pa_agent::types::Message::User(
            pa_agent::types::UserMessage {
                content: pa_agent::types::UserContent::Text("hi".to_string()),
                timestamp: timestamp_ms as i64,
            },
        ))
    }

    fn row_text(row: &AgentMessage) -> String {
        let AgentMessage::Custom(custom) = row else {
            panic!("custom row");
        };
        custom.payload["content"]
            .as_str()
            .expect("text content")
            .to_string()
    }

    #[test]
    fn first_turn_reports_the_time_without_a_previous_message() {
        let row = due_time_context_row(&[], NOW_MS).expect("first turn is due");
        assert_eq!(
            row_text(&row),
            "[time-context]\n\nCurrent time: 2026-11-19T14:32:05Z (UTC).\nElapsed since the previous message: none (this is the first message of the session)."
        );
    }

    #[test]
    fn a_return_after_a_gap_reports_the_elapsed_time() {
        let clock = due_time_context_row(&[], NOW_MS - 3 * 86_400_000).expect("due");
        let last_reply = NOW_MS - (2 * 3600 + 13 * 60 + 40) * 1000;
        let row = due_time_context_row(&[clock, user_at(last_reply)], NOW_MS).expect("due");
        assert!(row_text(&row).ends_with("Elapsed since the previous message: 2h 13m 40s."));
    }

    #[test]
    fn a_row_inside_the_refresh_interval_suppresses_the_next() {
        let clock = due_time_context_row(&[], NOW_MS - REFRESH_INTERVAL_MS + 1).expect("due");
        assert_eq!(
            due_time_context_row(&[clock, user_at(NOW_MS)], NOW_MS),
            None
        );
    }
}
