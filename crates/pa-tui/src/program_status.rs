//! OSC 7501 (Program Status Protocol) reports: the terminal's record of
//! what this program is doing.
//! <https://www.superlogical.com/rex/docs/build/program-status>
//!
//! Reports are written only once the terminal answered the protocol's
//! feature detection (the query rides the terminal probe, see
//! `enhanced_keys`); until then, and on terminals that did not answer,
//! nothing is written. The program never clears its record: on exit the
//! terminal drops `working` itself and keeps `done`/`error` for the user
//! (the protocol's "Lifetime").

use std::io::Write;

/// The app key every report repeats (each report replaces its record).
const APP: &str = "prime-agent";

/// The protocol's cap on a report's decoded `msg`.
const MESSAGE_MAX_BYTES: usize = 2048;

/// The states a detection reply without `states=` accepts (fixed by the
/// protocol's "Feature detection").
const BASE_STATES: [&str; 5] = ["idle", "working", "done", "blocked", "error"];

/// The statuses the TUI reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Working,
    Idle,
    Done,
    Error,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Idle => "idle",
            Status::Done => "done",
            Status::Error => "error",
        }
    }
}

/// The terminal's answer to the feature detection.
#[derive(Debug)]
enum Support {
    Pending,
    Unsupported,
    /// The states the terminal accepts.
    Supported(Vec<String>),
}

#[derive(Debug)]
struct Reporter {
    support: Support,
    /// The status the surface currently reports.
    current: Option<Status>,
    /// The line the current status carries as its `msg` (empty: none).
    message: String,
    /// The state word and message the terminal last received.
    written: Option<(&'static str, String)>,
}

impl Reporter {
    /// The state word owed to the terminal: the current status, or `idle`
    /// when the terminal does not accept it (the protocol's fallback to a
    /// listed state), unless the terminal already has it with this message.
    /// Every event-loop pass asks, so the check builds nothing.
    fn owed(&self) -> Option<&'static str> {
        let Support::Supported(states) = &self.support else {
            return None;
        };
        let accepted = |word: &&str| states.iter().any(|state| state == word);
        let word = [self.current?.word(), Status::Idle.word()]
            .into_iter()
            .find(accepted)?;
        let unchanged = self
            .written
            .as_ref()
            .is_some_and(|(written, message)| *written == word && *message == self.message);
        (!unchanged).then_some(word)
    }

    fn flush(&mut self) {
        let Some(word) = self.owed() else {
            return;
        };
        let mut out = std::io::stdout();
        if out
            .write_all(sequence(word, &self.message).as_bytes())
            .and_then(|()| out.flush())
            .is_ok()
        {
            self.written = Some((word, self.message.clone()));
        }
    }
}

/// The report for `word`. A non-empty `message` rides as `msg`: control
/// characters become spaces and the line is cut to the protocol's cap,
/// since a terminal discards a report whose `msg` breaks either rule.
fn sequence(word: &str, message: &str) -> String {
    use base64::Engine;
    let mut line: String = message
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if line.len() > MESSAGE_MAX_BYTES {
        let cut = (0..=MESSAGE_MAX_BYTES)
            .rev()
            .find(|&at| line.is_char_boundary(at))
            .unwrap_or(0);
        line.truncate(cut);
    }
    if line.is_empty() {
        return format!("\x1b]7501;state={word}:app={APP}\x1b\\");
    }
    let msg = base64::engine::general_purpose::STANDARD.encode(line);
    format!("\x1b]7501;state={word}:app={APP}:msg={msg}\x1b\\")
}

static REPORTER: std::sync::Mutex<Reporter> = std::sync::Mutex::new(Reporter {
    support: Support::Pending,
    current: None,
    message: String::new(),
    written: None,
});

fn reporter() -> std::sync::MutexGuard<'static, Reporter> {
    REPORTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Report the surface's status with `message` as its one-line `msg`
/// (empty: none).
pub(crate) fn report(status: Status, message: &str) {
    let mut reporter = reporter();
    reporter.current = Some(status);
    message.clone_into(&mut reporter.message);
    reporter.flush();
}

/// Record the feature detection's outcome (`None`: unsupported) and send
/// the current status to a supporting terminal.
pub(crate) fn detected(reply: Option<crossterm::terminal::ProgramStatusSupport>) {
    let mut reporter = reporter();
    reporter.support = match reply {
        Some(reply) => Support::Supported(
            reply
                .states
                .unwrap_or_else(|| BASE_STATES.map(str::to_string).to_vec()),
        ),
        None => Support::Unsupported,
    };
    reporter.flush();
}

/// Stand the reports down for the process exit: a detection answer that
/// lands during the exit restore records its support but writes nothing,
/// so on exit the terminal keeps the record it has.
pub(crate) fn release_for_exit() {
    reporter().current = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reporter(support: Support, current: Status) -> Reporter {
        Reporter {
            support,
            current: Some(current),
            message: String::new(),
            written: None,
        }
    }

    fn supported(states: &[&str]) -> Support {
        Support::Supported(states.iter().map(ToString::to_string).collect())
    }

    #[test]
    fn only_a_supporting_terminal_is_owed_a_report() {
        assert_eq!(reporter(Support::Pending, Status::Working).owed(), None);
        assert_eq!(reporter(Support::Unsupported, Status::Working).owed(), None);
        let mut supporting = reporter(supported(&BASE_STATES), Status::Working);
        assert_eq!(supporting.owed(), Some("working"));
        supporting.written = Some(("working", String::new()));
        assert_eq!(supporting.owed(), None, "an unchanged status is not resent");
        supporting.message = "renamed".to_string();
        assert_eq!(
            supporting.owed(),
            Some("working"),
            "a new message is resent"
        );
    }

    #[test]
    fn an_unaccepted_state_falls_back_to_idle_or_nothing() {
        let without_done = supported(&["idle", "working", "blocked", "error"]);
        assert_eq!(reporter(without_done, Status::Done).owed(), Some("idle"));
        assert_eq!(reporter(supported(&["working"]), Status::Done).owed(), None);
    }

    #[test]
    fn a_detection_answer_after_the_exit_release_writes_nothing() {
        let _lock = crate::enhanced_keys::TEST_STATE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *super::reporter() = Reporter {
            support: Support::Pending,
            current: None,
            message: String::new(),
            written: None,
        };
        report(Status::Working, "");
        crate::enhanced_keys::release_for_exit();
        detected(Some(crossterm::terminal::ProgramStatusSupport {
            states: None,
            kinds: None,
        }));
        assert_eq!(super::reporter().written, None);
    }

    #[test]
    fn the_message_rides_as_one_base64_line_within_the_cap() {
        use base64::Engine;
        assert_eq!(
            sequence("done", " fix\tlogin\n"),
            "\x1b]7501;state=done:app=prime-agent:msg=Zml4IGxvZ2lu\x1b\\"
        );
        // One ASCII byte ahead of two-byte characters puts the cap mid-character.
        let long = format!("a{}", "\u{e9}".repeat(MESSAGE_MAX_BYTES));
        let encoded = sequence("done", &long);
        let msg = encoded
            .strip_prefix("\x1b]7501;state=done:app=prime-agent:msg=")
            .and_then(|rest| rest.strip_suffix("\x1b\\"))
            .unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(msg)
            .unwrap();
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            format!("a{}", "\u{e9}".repeat(MESSAGE_MAX_BYTES / 2 - 1))
        );
    }
}
