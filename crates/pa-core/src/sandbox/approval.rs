//! Interactive tool-approval classification and per-session gating for `python_repl` cells.
//!
//! Classifies Python cell source into read-only, file-edit, bash-command (normal or risky),
//! or opaque/bypass Python, and gates execution through [`SessionApprovalGate`] when an
//! interactive emitter is attached and the session sandbox mode requires confirmation.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pa_types::sandbox::{
    SandboxMode, SandboxNetworkMode, ToolApprovalDecision, ToolApprovalKind, ToolApprovalRequest,
};

use crate::tools::tool_definition::AbortSignal;

/// Callback used to deliver an approval prompt to the interactive UI.
pub type ToolApprovalEmitter = Arc<dyn Fn(ToolApprovalRequest) + Send + Sync>;

/// Static classification of a `python_repl` cell before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellClassification {
    /// Safe read-only Python or read-only allowlisted `bash()` command.
    ReadOnly,
    /// File mutation via `edit()`, `Path.write_text()`, `open(..., 'w')`, etc.
    FileEdit { summary: String },
    /// Literal `bash()` command outside the read-only allowlist or flagged as risky.
    BashCommand {
        command: String,
        prefix: Option<String>,
        risky_reason: Option<String>,
    },
    /// Opaque or bypass-capable Python (`subprocess`, `os.remove`, dynamic `bash(var)`, etc.).
    OpaquePython { reason: String },
}

/// Outcome of [`SessionApprovalGate::check_and_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// Cell is approved to run.
    Allowed,
    /// Cell is approved and the user requested switching the session to yolo (`DangerFullAccess`).
    AllowedAndSwitchToYolo,
    /// Cell was denied or aborted with a user-facing reason.
    Denied(String),
}

const MAX_DETAIL_CHARS: usize = 500;

/// Patterns that indicate opaque or OS/network-bypassing Python.
///
/// Entries ending in `.` or `(` only require a leading word boundary; bare module
/// or attribute names require both leading and trailing word boundaries.
const OPAQUE_PYTHON_PATTERNS: &[(&str, &str)] = &[
    ("subprocess", "uses subprocess"),
    ("os.system", "uses os.system"),
    ("os.popen", "uses os.popen"),
    ("os.remove", "uses os.remove"),
    ("os.unlink", "uses os.unlink"),
    ("os.rmdir", "uses os.rmdir"),
    ("os.rename", "uses os.rename"),
    ("os.replace", "uses os.replace"),
    ("os.chmod", "uses os.chmod"),
    ("os.chown", "uses os.chown"),
    ("os.kill", "uses os.kill"),
    ("shutil.", "uses shutil"),
    ("exec(", "uses exec()"),
    ("eval(", "uses eval()"),
    ("__import__(", "uses __import__()"),
    ("ctypes", "uses ctypes"),
    ("urllib.", "uses urllib"),
    ("requests.", "uses requests"),
    ("httpx.", "uses httpx"),
    ("socket.", "uses socket"),
    ("aiohttp", "uses aiohttp"),
    ("http.client", "uses http.client"),
];

const OPAQUE_MODULES_FOR_IMPORT: &[(&str, &str)] = &[
    ("shutil", "imports shutil"),
    ("urllib", "imports urllib"),
    ("requests", "imports requests"),
    ("httpx", "imports httpx"),
    ("socket", "imports socket"),
];

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "pwd", "cat", "head", "tail", "wc", "find", "rg", "grep", "fd", "tree", "file", "stat",
    "which", "echo", "printf", "env", "uname", "whoami", "date",
];

const READ_ONLY_GIT_SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "branch",
    "show",
    "rev-parse",
    "ls-files",
];

const SUBCOMMAND_TOOLS: &[&str] = &[
    "git", "cargo", "npm", "pnpm", "yarn", "uv", "python", "python3", "pip", "pip3", "docker",
    "kubectl", "gh", "rustup", "go", "make", "just", "brew",
];

/// Classify a Python cell to determine whether it requires interactive approval.
#[must_use]
pub fn classify_python_cell(code: &str, cwd: &Path) -> CellClassification {
    let stripped = strip_comments_and_docstrings(code);
    let masked = mask_string_literals(&stripped);

    if let Some(reason) = detect_opaque_python(&masked) {
        return CellClassification::OpaquePython { reason };
    }

    match detect_bash_calls(&stripped, cwd) {
        BashScanResult::Opaque(reason) => return CellClassification::OpaquePython { reason },
        BashScanResult::Command {
            command,
            prefix,
            risky_reason,
        } => {
            return CellClassification::BashCommand {
                command,
                prefix,
                risky_reason,
            };
        }
        BashScanResult::ReadOnlyOrNone => {}
    }

    if let Some(summary) = detect_file_edit(&stripped, &masked) {
        return CellClassification::FileEdit { summary };
    }

    CellClassification::ReadOnly
}

/// Strip `#` comments (outside string literals) and standalone triple-quoted docstrings.
fn strip_comments_and_docstrings(code: &str) -> String {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    let mut at_stmt_start = true;

    while i < len {
        let b = bytes[i];
        if b == b'#' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'\'' || b == b'"' {
            let is_triple = i + 2 < len && bytes[i + 1] == b && bytes[i + 2] == b;
            if is_triple {
                let start = i;
                i += 3;
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2;
                        continue;
                    }
                    if i + 2 < len && bytes[i] == b && bytes[i + 1] == b && bytes[i + 2] == b {
                        i += 3;
                        break;
                    }
                    i += 1;
                }
                if at_stmt_start {
                    // Docstring: preserve newlines only so line structure stays intact.
                    for &ch in &bytes[start..i] {
                        if ch == b'\n' {
                            out.push('\n');
                        }
                    }
                } else {
                    out.push_str(&code[start..i]);
                }
                at_stmt_start = false;
                continue;
            }
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if bytes[i] == b {
                    i += 1;
                    break;
                }
                if bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            out.push_str(&code[start..i]);
            at_stmt_start = false;
            continue;
        }

        let ch = code[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
        if ch == '\n' || ch == ':' || ch == ';' {
            at_stmt_start = true;
        } else if !ch.is_ascii_whitespace() {
            at_stmt_start = false;
        }
    }

    out
}

/// Replace string literal contents with spaces so identifier scans never match inside strings.
fn mask_string_literals(code: &str) -> String {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        let b = bytes[i];
        if b == b'\'' || b == b'"' {
            let is_triple = i + 2 < len && bytes[i + 1] == b && bytes[i + 2] == b;
            let quote = b as char;
            if is_triple {
                out.push(quote);
                out.push(quote);
                out.push(quote);
                i += 3;
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        out.push(' ');
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    if i + 2 < len && bytes[i] == b && bytes[i + 1] == b && bytes[i + 2] == b {
                        out.push(quote);
                        out.push(quote);
                        out.push(quote);
                        i += 3;
                        break;
                    }
                    if bytes[i] == b'\n' {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                    i += 1;
                }
            } else {
                out.push(quote);
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        out.push(' ');
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b {
                        out.push(quote);
                        i += 1;
                        break;
                    }
                    if bytes[i] == b'\n' {
                        out.push('\n');
                        i += 1;
                        break;
                    }
                    out.push(' ');
                    i += 1;
                }
            }
            continue;
        }
        let ch = code[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn contains_token_pattern(code: &str, pattern: &str) -> bool {
    let bytes = code.as_bytes();
    let pat_bytes = pattern.as_bytes();
    let needs_trailing_boundary = pat_bytes.last().is_some_and(|&last| is_ident_byte(last));

    let mut search_from = 0;
    while let Some(rel) = code[search_from..].find(pattern) {
        let start = search_from + rel;
        let end = start + pat_bytes.len();
        let leading_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let trailing_ok =
            !needs_trailing_boundary || end == bytes.len() || !is_ident_byte(bytes[end]);
        if leading_ok && trailing_ok {
            return true;
        }
        search_from = start + 1;
    }
    false
}

fn detect_opaque_python(masked_code: &str) -> Option<String> {
    for &(pattern, reason) in OPAQUE_PYTHON_PATTERNS {
        if contains_token_pattern(masked_code, pattern) {
            return Some(reason.to_string());
        }
    }
    for &(module, reason) in OPAQUE_MODULES_FOR_IMPORT {
        let import_pat = format!("import {module}");
        let from_pat = format!("from {module}");
        if contains_token_pattern(masked_code, &import_pat)
            || contains_token_pattern(masked_code, &from_pat)
        {
            return Some(reason.to_string());
        }
    }
    None
}

enum BashScanResult {
    ReadOnlyOrNone,
    Command {
        command: String,
        prefix: Option<String>,
        risky_reason: Option<String>,
    },
    Opaque(String),
}

/// Find every `bash(...)` call in stripped Python source and classify it.
fn detect_bash_calls(code: &str, cwd: &Path) -> BashScanResult {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut best_command: Option<(String, Option<String>, Option<String>)> = None;

    while i < len {
        // Skip string literals so we never match `bash(` inside a string.
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            i = skip_python_string(bytes, i);
            continue;
        }
        if code[i..].starts_with("bash") {
            let leading_ok = i == 0 || (!is_ident_byte(bytes[i - 1]) && bytes[i - 1] != b'.');
            let after_name = i + 4;
            if leading_ok && (after_name == len || !is_ident_byte(bytes[after_name])) {
                let mut paren_idx = after_name;
                while paren_idx < len && bytes[paren_idx].is_ascii_whitespace() {
                    paren_idx += 1;
                }
                if paren_idx < len && bytes[paren_idx] == b'(' {
                    let Some(close_idx) = find_matching_paren(bytes, paren_idx) else {
                        return BashScanResult::Opaque("dynamic bash() command".to_string());
                    };
                    let args_str = &code[paren_idx + 1..close_idx];
                    let Some(cmd) = extract_literal_bash_arg(args_str) else {
                        return BashScanResult::Opaque("dynamic bash() command".to_string());
                    };
                    if let Some((prefix, risky_reason)) = classify_bash_command(&cmd, cwd) {
                        let should_replace = match &best_command {
                            None => true,
                            Some((_, _, existing_risk)) => {
                                existing_risk.is_none() && risky_reason.is_some()
                            }
                        };
                        if should_replace {
                            best_command = Some((cmd, prefix, risky_reason));
                        }
                    }
                    i = close_idx + 1;
                    continue;
                }
            }
        }
        i += code[i..].chars().next().map_or(1, char::len_utf8);
    }

    match best_command {
        Some((command, prefix, risky_reason)) => BashScanResult::Command {
            command,
            prefix,
            risky_reason,
        },
        None => BashScanResult::ReadOnlyOrNone,
    }
}

fn skip_python_string(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let q = bytes[start];
    if start + 2 < len && bytes[start + 1] == q && bytes[start + 2] == q {
        let mut i = start + 3;
        while i < len {
            if bytes[i] == b'\\' && i + 1 < len {
                i += 2;
                continue;
            }
            if i + 2 < len && bytes[i] == q && bytes[i + 1] == q && bytes[i + 2] == q {
                return i + 3;
            }
            i += 1;
        }
        return len;
    }
    let mut i = start + 1;
    while i < len {
        if bytes[i] == b'\\' && i + 1 < len {
            i += 2;
            continue;
        }
        if bytes[i] == q {
            return i + 1;
        }
        if bytes[i] == b'\n' {
            return i + 1;
        }
        i += 1;
    }
    len
}

fn find_matching_paren(bytes: &[u8], open_idx: usize) -> Option<usize> {
    let len = bytes.len();
    let mut depth = 0usize;
    let mut i = open_idx;
    while i < len {
        let b = bytes[i];
        if b == b'\'' || b == b'"' {
            i = skip_python_string(bytes, i);
            continue;
        }
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split top-level comma-separated arguments inside a Python call.
fn split_python_args(args: &str) -> Vec<&str> {
    let bytes = args.as_bytes();
    let len = bytes.len();
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;

    while i < len {
        let b = bytes[i];
        if b == b'\'' || b == b'"' {
            i = skip_python_string(bytes, i);
            continue;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(args[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = args[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

/// Parse a standalone Python string literal (no f-strings, no concatenation, no `.format`).
fn parse_python_string_literal(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut prefix_end = 0;
    while prefix_end < bytes.len() && bytes[prefix_end].is_ascii_alphabetic() {
        prefix_end += 1;
    }
    let prefix = &trimmed[..prefix_end];
    let mut is_raw = false;
    for ch in prefix.chars() {
        match ch.to_ascii_lowercase() {
            'r' => is_raw = true,
            'u' => {}
            // f-strings or byte strings are dynamic / non-plain
            _ => return None,
        }
    }
    if prefix_end >= bytes.len() {
        return None;
    }
    let q = bytes[prefix_end];
    if q != b'\'' && q != b'"' {
        return None;
    }
    let end = skip_python_string(bytes, prefix_end);
    // Reject anything trailing after the literal (`"a" + b`, `"a".format(...)`, `"a" % x`).
    if end != bytes.len() {
        return None;
    }
    let is_triple =
        prefix_end + 2 < bytes.len() && bytes[prefix_end + 1] == q && bytes[prefix_end + 2] == q;
    let delim_len = if is_triple { 3 } else { 1 };
    if end < prefix_end + delim_len * 2 {
        return None;
    }
    let inner = &trimmed[prefix_end + delim_len..end - delim_len];
    if is_raw {
        return Some(inner.to_string());
    }
    let mut unescaped = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => unescaped.push('\n'),
                Some('r') => unescaped.push('\r'),
                Some('t') => unescaped.push('\t'),
                Some('\\') | None => unescaped.push('\\'),
                Some('\'') => unescaped.push('\''),
                Some('"') => unescaped.push('"'),
                Some(other) => {
                    unescaped.push('\\');
                    unescaped.push(other);
                }
            }
        } else {
            unescaped.push(ch);
        }
    }
    Some(unescaped)
}

fn extract_literal_bash_arg(args_str: &str) -> Option<String> {
    let args = split_python_args(args_str);
    if args.is_empty() {
        return None;
    }
    // Check explicit keyword argument `cmd=` or `command=` first.
    for arg in &args {
        if let Some((key, value)) = arg.split_once('=') {
            let key = key.trim();
            if key == "cmd" || key == "command" {
                return parse_python_string_literal(value);
            }
        }
    }
    // Otherwise require the first argument to be a plain positional string literal.
    let first = args[0];
    if first.contains('=') && !first.starts_with('\'') && !first.starts_with('"') {
        return None;
    }
    parse_python_string_literal(first)
}

/// Returns `None` when the command is strictly read-only, or `Some((prefix, risky_reason))`.
fn classify_bash_command(command: &str, cwd: &Path) -> Option<(Option<String>, Option<String>)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut risky_reason = detect_shell_syntax_risks(trimmed);
    let segments = split_shell_segments(trimmed);
    if segments.is_empty() {
        return None;
    }

    // Check pipe-to-shell across piped segments (`| sh`, `| bash`, `| zsh`).
    if risky_reason.is_none() {
        for &(seg, preceded_by_pipe) in &segments {
            if preceded_by_pipe {
                let words = tokenize_shell_words(seg);
                let cmd = skip_env_assignments(&words).first().map(String::as_str);
                if matches!(
                    cmd,
                    Some("sh" | "bash" | "zsh" | "/bin/sh" | "/bin/bash" | "/bin/zsh")
                ) {
                    risky_reason = Some("pipes output to a shell".to_string());
                    break;
                }
            }
        }
    }

    let mut all_read_only = risky_reason.is_none();
    let mut first_non_readonly_prefix: Option<String> = None;
    let mut first_prefix: Option<String> = None;

    for &(seg, _) in &segments {
        let words = tokenize_shell_words(seg);
        let cmd_words = skip_env_assignments(&words);
        if cmd_words.is_empty() {
            continue;
        }
        let seg_prefix = extract_prefix(cmd_words);
        if first_prefix.is_none() {
            first_prefix.clone_from(&seg_prefix);
        }
        if risky_reason.is_none() {
            if let Some(reason) = detect_segment_risk(cmd_words, cwd) {
                risky_reason = Some(reason);
                all_read_only = false;
            }
        }
        if !is_read_only_segment(cmd_words) {
            all_read_only = false;
            if first_non_readonly_prefix.is_none() {
                first_non_readonly_prefix = seg_prefix;
            }
        }
    }

    if all_read_only && risky_reason.is_none() {
        return None;
    }

    let prefix = first_non_readonly_prefix.or(first_prefix);
    Some((prefix, risky_reason))
}

/// Detect shell metacharacters outside quotes: redirections (`>`/`>>`), `$(`, backticks.
fn detect_shell_syntax_risks(command: &str) -> Option<String> {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < len {
        let b = bytes[i];
        if b == b'\\' && !in_single && i + 1 < len {
            i += 2;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if b == b'"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if !in_single {
            if b == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                return Some("uses command substitution $()".to_string());
            }
            if b == b'`' {
                return Some("uses backtick command substitution".to_string());
            }
        }
        if !in_single && !in_double && b == b'>' {
            return Some("uses output redirection".to_string());
        }
        i += 1;
    }
    None
}

/// Split a shell command on `&&`, `||`, `;`, `|` outside quotes, tracking whether each
/// segment was immediately preceded by a single pipe `|`.
fn split_shell_segments(command: &str) -> Vec<(&str, bool)> {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut preceded_by_pipe = false;

    while i < len {
        let b = bytes[i];
        if b == b'\\' && !in_single && i + 1 < len {
            i += 2;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if b == b'"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if !in_single && !in_double {
            let (sep_len, is_pipe) = if b == b';' {
                (1, false)
            } else if i + 1 < len
                && ((b == b'&' && bytes[i + 1] == b'&') || (b == b'|' && bytes[i + 1] == b'|'))
            {
                (2, false)
            } else if b == b'|' {
                (1, true)
            } else {
                (0, false)
            };
            if sep_len > 0 {
                let seg = command[start..i].trim();
                if !seg.is_empty() {
                    out.push((seg, preceded_by_pipe));
                }
                preceded_by_pipe = is_pipe;
                i += sep_len;
                start = i;
                continue;
            }
        }
        i += 1;
    }

    let tail = command[start..].trim();
    if !tail.is_empty() {
        out.push((tail, preceded_by_pipe));
    }
    out
}

fn tokenize_shell_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = segment.chars();

    while let Some(ch) = chars.next() {
        if ch == '\\' && !in_single {
            if let Some(next) = chars.next() {
                current.push(next);
                in_word = true;
            }
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            in_word = true;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            in_word = true;
            continue;
        }
        if !in_single && !in_double && ch.is_ascii_whitespace() {
            if in_word {
                words.push(std::mem::take(&mut current));
                in_word = false;
            }
            continue;
        }
        current.push(ch);
        in_word = true;
    }
    if in_word {
        words.push(current);
    }
    words
}

fn skip_env_assignments(words: &[String]) -> &[String] {
    let mut idx = 0;
    while idx < words.len() {
        let word = &words[idx];
        if let Some((key, _)) = word.split_once('=') {
            if !key.is_empty()
                && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !key.as_bytes()[0].is_ascii_digit()
            {
                idx += 1;
                continue;
            }
        }
        break;
    }
    &words[idx..]
}

fn extract_prefix(words: &[String]) -> Option<String> {
    let first = words.first()?;
    if SUBCOMMAND_TOOLS.contains(&first.as_str()) {
        if let Some(second) = words.get(1) {
            if !second.starts_with('-') {
                return Some(format!("{first} {second}"));
            }
        }
    }
    Some(first.clone())
}

fn detect_segment_risk(words: &[String], cwd: &Path) -> Option<String> {
    let cmd = words.first()?.as_str();
    let args = &words[1..];

    match cmd {
        "rm" => {
            let recursive = args.iter().any(|arg| {
                arg == "--recursive"
                    || (arg.starts_with('-')
                        && !arg.starts_with("--")
                        && (arg.contains('r') || arg.contains('R')))
            });
            if recursive {
                return Some("recursive rm command".to_string());
            }
        }
        "git" => {
            if let Some(sub) = args.first().map(String::as_str) {
                let rest = &args[1..];
                if sub == "reset" && rest.iter().any(|a| a == "--hard") {
                    return Some("git reset --hard".to_string());
                }
                if sub == "push"
                    && rest
                        .iter()
                        .any(|a| a == "--force" || a == "-f" || a == "--force-with-lease")
                {
                    return Some("git force push".to_string());
                }
                if sub == "clean"
                    && rest.iter().any(|a| {
                        a == "--force"
                            || (a.starts_with('-') && !a.starts_with("--") && a.contains('f'))
                    })
                {
                    return Some("git clean -f".to_string());
                }
            }
        }
        "sudo" => return Some("uses sudo".to_string()),
        "chmod" => return Some("changes file permissions (chmod)".to_string()),
        "chown" => return Some("changes file ownership (chown)".to_string()),
        "kill" | "pkill" => return Some(format!("terminates processes ({cmd})")),
        "dd" => return Some("raw disk/stream operation (dd)".to_string()),
        _ if cmd == "mkfs" || cmd.starts_with("mkfs.") => {
            return Some("creates a filesystem (mkfs)".to_string());
        }
        "curl" | "wget" => return Some(format!("network request ({cmd})")),
        "ssh" | "scp" => return Some(format!("remote access ({cmd})")),
        "pip" | "pip3" | "npm" | "brew" if args.first().map(String::as_str) == Some("install") => {
            return Some(format!("installs packages ({cmd} install)"));
        }
        _ => {}
    }

    for token in args {
        let candidate = token
            .split_once('=')
            .map_or(token.as_str(), |(_, value)| value);
        if let Some(reason) = detect_outside_cwd_path(candidate, cwd) {
            return Some(reason);
        }
    }

    None
}

fn detect_outside_cwd_path(token: &str, cwd: &Path) -> Option<String> {
    if token.is_empty() || token == "/dev/null" {
        return None;
    }
    if token == ".." || token.starts_with("../") || token.ends_with("/..") || token.contains("/../")
    {
        return Some(format!(
            "references parent path outside workspace ({token})"
        ));
    }
    if token == "~" || token.starts_with("~/") {
        return Some(format!("references home path outside workspace ({token})"));
    }
    if token.starts_with('/') {
        let path = Path::new(token);
        if !path.starts_with(cwd) {
            return Some(format!("references path outside workspace ({token})"));
        }
    }
    None
}

fn is_read_only_segment(words: &[String]) -> bool {
    let Some(cmd) = words.first().map(String::as_str) else {
        return true;
    };
    let args = &words[1..];
    if cmd == "git" {
        let Some(sub) = args.first().map(String::as_str) else {
            return false;
        };
        if !READ_ONLY_GIT_SUBCOMMANDS.contains(&sub) {
            return false;
        }
        if sub == "branch" {
            let mutating = args[1..].iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-d" | "-D" | "-m" | "-M" | "-c" | "-C" | "--delete" | "--move" | "--copy"
                )
            });
            if mutating {
                return false;
            }
        }
        return true;
    }
    if !READ_ONLY_COMMANDS.contains(&cmd) {
        return false;
    }
    if cmd == "find" {
        let mutating = args.iter().any(|a| {
            matches!(
                a.as_str(),
                "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
            )
        });
        if mutating {
            return false;
        }
    }
    true
}

fn detect_file_edit(stripped_code: &str, masked_code: &str) -> Option<String> {
    let edit_patterns: &[(&str, &str)] = &[
        ("edit(", "edit() file modification"),
        (".write_text(", "Path.write_text()"),
        (".write_bytes(", "Path.write_bytes()"),
        (".unlink(", "Path.unlink()"),
        (".mkdir(", "Path.mkdir()"),
        (".rmdir(", "Path.rmdir()"),
        (".rename(", "Path.rename()"),
    ];
    for &(pattern, summary) in edit_patterns {
        if pattern.starts_with('.') {
            if masked_code.contains(pattern) {
                return Some(summary.to_string());
            }
        } else if contains_token_pattern(masked_code, pattern) {
            return Some(summary.to_string());
        }
    }
    if has_mutating_open_call(stripped_code) {
        return Some("open() write/append".to_string());
    }
    None
}

fn has_mutating_open_call(code: &str) -> bool {
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            i = skip_python_string(bytes, i);
            continue;
        }
        if code[i..].starts_with("open") {
            let leading_ok = i == 0 || (!is_ident_byte(bytes[i - 1]) && bytes[i - 1] != b'.');
            let after_name = i + 4;
            if leading_ok && (after_name == len || !is_ident_byte(bytes[after_name])) {
                let mut paren_idx = after_name;
                while paren_idx < len && bytes[paren_idx].is_ascii_whitespace() {
                    paren_idx += 1;
                }
                if paren_idx < len && bytes[paren_idx] == b'(' {
                    if let Some(close_idx) = find_matching_paren(bytes, paren_idx) {
                        let args = split_python_args(&code[paren_idx + 1..close_idx]);
                        let mode_expr = args
                            .iter()
                            .find_map(|arg| {
                                let (k, v) = arg.split_once('=')?;
                                (k.trim() == "mode").then_some(v)
                            })
                            .or_else(|| args.get(1).copied().filter(|arg| !arg.contains('=')));
                        if let Some(mode_expr) = mode_expr {
                            if let Some(mode) = parse_python_string_literal(mode_expr) {
                                if mode.contains('w')
                                    || mode.contains('a')
                                    || mode.contains('x')
                                    || mode.contains('+')
                                {
                                    return true;
                                }
                            }
                        }
                        i = close_idx + 1;
                        continue;
                    }
                }
            }
        }
        i += code[i..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn truncate_detail(code: &str) -> String {
    let trimmed = code.trim();
    if trimmed.chars().count() <= MAX_DETAIL_CHARS {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(MAX_DETAIL_CHARS).collect();
    format!("{prefix}...")
}

struct GateState {
    mode: SandboxMode,
    network: SandboxNetworkMode,
    allow_edits: bool,
    allowed_bash_prefixes: HashSet<String>,
    pending: HashMap<String, tokio::sync::oneshot::Sender<ToolApprovalDecision>>,
    emitter: Option<ToolApprovalEmitter>,
}

/// Thread-safe per-session approval gate for `python_repl` cells.
///
/// Immediately allows cells when mode is [`SandboxMode::Off`] or
/// [`SandboxMode::DangerFullAccess`], or when no interactive emitter is attached
/// (so headless runs, tests, and subagents never hang).
pub struct SessionApprovalGate {
    state: Mutex<GateState>,
    next_id: AtomicU64,
}

impl SessionApprovalGate {
    /// Create a new gate with the given sandbox posture and optional interactive emitter.
    #[must_use]
    pub fn new(
        mode: SandboxMode,
        network: SandboxNetworkMode,
        emitter: Option<ToolApprovalEmitter>,
    ) -> Self {
        Self {
            state: Mutex::new(GateState {
                mode,
                network,
                allow_edits: false,
                allowed_bash_prefixes: HashSet::new(),
                pending: HashMap::new(),
                emitter,
            }),
            next_id: AtomicU64::new(1),
        }
    }

    /// Attach or clear the interactive emitter.
    pub fn set_emitter(&self, emitter: Option<ToolApprovalEmitter>) {
        self.lock_state().emitter = emitter;
    }

    /// Update the sandbox mode (and optionally network posture), returning the new pair.
    pub fn set_mode(
        &self,
        mode: SandboxMode,
        network: Option<SandboxNetworkMode>,
    ) -> (SandboxMode, SandboxNetworkMode) {
        let mut state = self.lock_state();
        state.mode = mode;
        if let Some(network) = network {
            state.network = network;
        }
        (state.mode, state.network)
    }

    /// Current `(mode, network)` posture.
    #[must_use]
    pub fn state(&self) -> (SandboxMode, SandboxNetworkMode) {
        let state = self.lock_state();
        (state.mode, state.network)
    }

    /// Resolve a pending approval request by id. Returns `true` if a waiting request was resolved.
    pub fn resolve(&self, request_id: &str, decision: ToolApprovalDecision) -> bool {
        let sender = self.lock_state().pending.remove(request_id);
        sender.is_some_and(|tx| tx.send(decision).is_ok())
    }

    /// Check whether `code` requires approval under the current sandbox posture, emitting
    /// a [`ToolApprovalRequest`] and awaiting the user's decision (raced against `signal`).
    pub async fn check_and_request(
        &self,
        code: &str,
        cwd: &Path,
        signal: Option<AbortSignal>,
    ) -> ApprovalOutcome {
        let (mode, emitter, allow_edits, allowed_prefixes) = {
            let state = self.lock_state();
            if matches!(state.mode, SandboxMode::Off | SandboxMode::DangerFullAccess) {
                return ApprovalOutcome::Allowed;
            }
            let Some(emitter) = state.emitter.clone() else {
                return ApprovalOutcome::Allowed;
            };
            (
                state.mode,
                emitter,
                state.allow_edits,
                state.allowed_bash_prefixes.clone(),
            )
        };
        let _ = mode;

        let classification = classify_python_cell(code, cwd);
        let request_id = format!("approval-{}", self.next_id.fetch_add(1, Ordering::Relaxed));

        let request = match classification {
            CellClassification::ReadOnly => return ApprovalOutcome::Allowed,
            CellClassification::FileEdit { summary } => {
                if allow_edits {
                    return ApprovalOutcome::Allowed;
                }
                ToolApprovalRequest {
                    request_id: request_id.clone(),
                    kind: ToolApprovalKind::FileEdit,
                    summary,
                    detail: truncate_detail(code),
                    risk_reason: None,
                    allow_prefix: None,
                }
            }
            CellClassification::BashCommand {
                command,
                prefix,
                risky_reason,
            } => {
                if risky_reason.is_none()
                    && prefix
                        .as_ref()
                        .is_some_and(|p| allowed_prefixes.contains(p))
                {
                    return ApprovalOutcome::Allowed;
                }
                let kind = if risky_reason.is_some() {
                    ToolApprovalKind::RiskyBashCommand
                } else {
                    ToolApprovalKind::BashCommand
                };
                let allow_prefix = if risky_reason.is_some() { None } else { prefix };
                ToolApprovalRequest {
                    request_id: request_id.clone(),
                    kind,
                    summary: command.clone(),
                    detail: command,
                    risk_reason: risky_reason,
                    allow_prefix,
                }
            }
            CellClassification::OpaquePython { reason } => ToolApprovalRequest {
                request_id: request_id.clone(),
                kind: ToolApprovalKind::OpaquePython,
                summary: reason.clone(),
                detail: truncate_detail(code),
                risk_reason: Some(reason),
                allow_prefix: None,
            },
        };

        if signal.as_ref().is_some_and(AbortSignal::is_cancelled) {
            return ApprovalOutcome::Denied("Approval aborted".to_string());
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.lock_state().pending.insert(request_id.clone(), tx);
        emitter(request);

        let decision = match signal {
            Some(signal) => {
                tokio::select! {
                    res = rx => res.ok(),
                    () = signal.cancelled() => {
                        self.lock_state().pending.remove(&request_id);
                        return ApprovalOutcome::Denied("Approval aborted".to_string());
                    }
                }
            }
            None => rx.await.ok(),
        };

        let Some(decision) = decision else {
            return ApprovalOutcome::Denied("Approval cancelled".to_string());
        };

        match decision {
            ToolApprovalDecision::AllowOnce => ApprovalOutcome::Allowed,
            ToolApprovalDecision::AllowSessionEdits => {
                self.lock_state().allow_edits = true;
                ApprovalOutcome::Allowed
            }
            ToolApprovalDecision::AllowSessionPrefix { prefix } => {
                if !prefix.is_empty() {
                    self.lock_state().allowed_bash_prefixes.insert(prefix);
                }
                ApprovalOutcome::Allowed
            }
            ToolApprovalDecision::SwitchToYolo => {
                self.lock_state().mode = SandboxMode::DangerFullAccess;
                ApprovalOutcome::AllowedAndSwitchToYolo
            }
            ToolApprovalDecision::Deny { reason } => {
                ApprovalOutcome::Denied(reason.unwrap_or_else(|| "Denied by user".to_string()))
            }
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ws() -> PathBuf {
        PathBuf::from("/workspace/project")
    }

    #[test]
    fn classifier_strips_comments_and_docstrings() {
        let code = r#"
"""
Do not run subprocess.run or os.remove here.
"""
# os.system("rm -rf /")
x = 1 + 2
print("subprocess is just text")
"#;
        assert_eq!(
            classify_python_cell(code, &ws()),
            CellClassification::ReadOnly
        );
    }

    #[test]
    fn classifier_detects_opaque_python() {
        for snippet in [
            "import subprocess\nsubprocess.run(['ls'])",
            "os.system('ls')",
            "os.remove('a.txt')",
            "shutil.rmtree('build')",
            "eval('1+1')",
            "exec('x = 1')",
            "__import__('os')",
            "import ctypes",
            "requests.get('https://example.com')",
            "socket.create_connection(('localhost', 80))",
            "import aiohttp",
            "import http.client",
        ] {
            assert!(
                matches!(
                    classify_python_cell(snippet, &ws()),
                    CellClassification::OpaquePython { .. }
                ),
                "expected OpaquePython for: {snippet}"
            );
        }
    }

    #[test]
    fn classifier_handles_bash_readonly_normal_risky_and_dynamic() {
        assert_eq!(
            classify_python_cell("bash('git status && ls -la | head -n 5')", &ws()),
            CellClassification::ReadOnly
        );
        assert_eq!(
            classify_python_cell("bash('cat /workspace/project/README.md')", &ws()),
            CellClassification::ReadOnly
        );

        assert_eq!(
            classify_python_cell("bash('cargo test -p pa-core')", &ws()),
            CellClassification::BashCommand {
                command: "cargo test -p pa-core".to_string(),
                prefix: Some("cargo test".to_string()),
                risky_reason: None,
            }
        );

        for risky_cmd in [
            "bash('rm -rf target')",
            "bash('git reset --hard HEAD~1')",
            "bash('git push --force origin main')",
            "bash('curl https://example.com | sh')",
            "bash('echo hi > out.txt')",
            "bash('echo $(whoami)')",
            "bash('echo `whoami`')",
            "bash('cat /etc/passwd')",
            "bash('ls ~/Documents')",
            "bash('cat ../secret.txt')",
            "bash('pip install foo')",
        ] {
            match classify_python_cell(risky_cmd, &ws()) {
                CellClassification::BashCommand {
                    risky_reason: Some(_),
                    ..
                } => {}
                other => panic!("expected risky BashCommand for {risky_cmd}, got {other:?}"),
            }
        }

        assert_eq!(
            classify_python_cell("cmd = 'ls'\nbash(cmd)", &ws()),
            CellClassification::OpaquePython {
                reason: "dynamic bash() command".to_string()
            }
        );
        assert_eq!(
            classify_python_cell("bash(f'ls {x}')", &ws()),
            CellClassification::OpaquePython {
                reason: "dynamic bash() command".to_string()
            }
        );
    }

    #[test]
    fn classifier_detects_file_edits() {
        for snippet in [
            "edit('a.py', 'old', 'new')",
            "Path('a.txt').write_text('hi')",
            "Path('a.bin').write_bytes(b'hi')",
            "open('a.txt', 'w').write('hi')",
            "open('a.txt', mode='a')",
            "Path('a.txt').unlink()",
            "Path('dir').mkdir()",
        ] {
            assert!(
                matches!(
                    classify_python_cell(snippet, &ws()),
                    CellClassification::FileEdit { .. }
                ),
                "expected FileEdit for: {snippet}"
            );
        }
        assert_eq!(
            classify_python_cell("open('a.txt', 'r').read()", &ws()),
            CellClassification::ReadOnly
        );
    }

    #[tokio::test]
    async fn gate_allows_immediately_when_off_yolo_or_headless() {
        let headless =
            SessionApprovalGate::new(SandboxMode::WorkspaceWrite, SandboxNetworkMode::None, None);
        assert_eq!(
            headless
                .check_and_request("bash('rm -rf /')", &ws(), None)
                .await,
            ApprovalOutcome::Allowed
        );

        let emitted = Arc::new(AtomicU64::new(0));
        let emitted_clone = Arc::clone(&emitted);
        let off_gate = SessionApprovalGate::new(
            SandboxMode::Off,
            SandboxNetworkMode::None,
            Some(Arc::new(move |_| {
                emitted_clone.fetch_add(1, Ordering::SeqCst);
            })),
        );
        assert_eq!(
            off_gate
                .check_and_request("bash('rm -rf /')", &ws(), None)
                .await,
            ApprovalOutcome::Allowed
        );
        assert_eq!(emitted.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn gate_handles_decisions_and_session_allows() {
        let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(SessionApprovalGate::new(
            SandboxMode::WorkspaceWrite,
            SandboxNetworkMode::None,
            Some(Arc::new(move |req| {
                let _ = req_tx.send(req);
            })),
        ));

        // 1. AllowOnce
        let g = Arc::clone(&gate);
        let handle =
            tokio::spawn(
                async move { g.check_and_request("bash('cargo test')", &ws(), None).await },
            );
        let req = req_rx.recv().await.unwrap();
        assert_eq!(req.kind, ToolApprovalKind::BashCommand);
        assert_eq!(req.allow_prefix.as_deref(), Some("cargo test"));
        assert!(gate.resolve(&req.request_id, ToolApprovalDecision::AllowOnce));
        assert_eq!(handle.await.unwrap(), ApprovalOutcome::Allowed);

        // 2. AllowSessionPrefix remembers prefix for subsequent calls
        let g = Arc::clone(&gate);
        let handle = tokio::spawn(async move {
            g.check_and_request("bash('cargo test -p pa-core')", &ws(), None)
                .await
        });
        let req = req_rx.recv().await.unwrap();
        assert!(gate.resolve(
            &req.request_id,
            ToolApprovalDecision::AllowSessionPrefix {
                prefix: "cargo test".to_string()
            }
        ));
        assert_eq!(handle.await.unwrap(), ApprovalOutcome::Allowed);
        // Second call with same prefix does not prompt.
        assert_eq!(
            gate.check_and_request("bash('cargo test --lib')", &ws(), None)
                .await,
            ApprovalOutcome::Allowed
        );

        // 3. AllowSessionEdits remembers edit approval
        let g = Arc::clone(&gate);
        let handle = tokio::spawn(async move {
            g.check_and_request("edit('f.rs', 'a', 'b')", &ws(), None)
                .await
        });
        let req = req_rx.recv().await.unwrap();
        assert_eq!(req.kind, ToolApprovalKind::FileEdit);
        assert!(gate.resolve(&req.request_id, ToolApprovalDecision::AllowSessionEdits));
        assert_eq!(handle.await.unwrap(), ApprovalOutcome::Allowed);
        assert_eq!(
            gate.check_and_request("Path('x.txt').write_text('hi')", &ws(), None)
                .await,
            ApprovalOutcome::Allowed
        );

        // 4. Deny returns Denied with reason
        let g = Arc::clone(&gate);
        let handle = tokio::spawn(async move {
            g.check_and_request("bash('rm -rf target')", &ws(), None)
                .await
        });
        let req = req_rx.recv().await.unwrap();
        assert_eq!(req.kind, ToolApprovalKind::RiskyBashCommand);
        assert!(req.allow_prefix.is_none());
        assert!(req.risk_reason.is_some());
        assert!(gate.resolve(
            &req.request_id,
            ToolApprovalDecision::Deny {
                reason: Some("too risky".to_string())
            }
        ));
        assert_eq!(
            handle.await.unwrap(),
            ApprovalOutcome::Denied("too risky".to_string())
        );

        // 5. SwitchToYolo updates mode and returns AllowedAndSwitchToYolo
        let g = Arc::clone(&gate);
        let handle =
            tokio::spawn(async move { g.check_and_request("os.remove('a')", &ws(), None).await });
        let req = req_rx.recv().await.unwrap();
        assert_eq!(req.kind, ToolApprovalKind::OpaquePython);
        assert!(gate.resolve(&req.request_id, ToolApprovalDecision::SwitchToYolo));
        assert_eq!(
            handle.await.unwrap(),
            ApprovalOutcome::AllowedAndSwitchToYolo
        );
        assert_eq!(
            gate.state(),
            (SandboxMode::DangerFullAccess, SandboxNetworkMode::None)
        );
    }

    #[tokio::test]
    async fn gate_aborts_pending_request_on_signal() {
        let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(SessionApprovalGate::new(
            SandboxMode::WorkspaceWrite,
            SandboxNetworkMode::None,
            Some(Arc::new(move |req| {
                let _ = req_tx.send(req);
            })),
        ));
        let signal = AbortSignal::new();
        let g = Arc::clone(&gate);
        let s = signal.clone();
        let handle = tokio::spawn(async move {
            g.check_and_request("bash('cargo build')", &ws(), Some(s))
                .await
        });
        let req = req_rx.recv().await.unwrap();
        signal.cancel();
        assert_eq!(
            handle.await.unwrap(),
            ApprovalOutcome::Denied("Approval aborted".to_string())
        );
        // Pending entry was cleaned up.
        assert!(!gate.resolve(&req.request_id, ToolApprovalDecision::AllowOnce));
    }
}
