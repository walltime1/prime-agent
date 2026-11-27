//! Kernel environment scrubbing: with the sandbox on, provider and cloud
//! credentials never enter the kernel process, so model-run code (and any
//! `bash()` it starts) cannot read them out of `os.environ`.

use std::collections::HashMap;
use std::ffi::OsString;

/// Substrings that mark a variable as a credential.
const SECRET_MARKERS: [&str; 5] = ["API_KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"];

/// Prefixes whose every variable is a credential.
const SECRET_PREFIXES: [&str; 6] = ["AWS_", "ANTHROPIC_", "OPENAI_", "GITHUB_", "GH_", "HF_"];

/// Variables kept even when they look secret: they configure the runtime
/// itself and carry no credential.
const KEEP_EXACT: [&str; 4] = [
    "GH_PAGER",
    "GITHUB_CLIENT_ID",
    "HF_HOME",
    "TOKENIZERS_PARALLELISM",
];

/// Remove credential-looking variables from the kernel environment.
pub fn scrub_secrets<S: std::hash::BuildHasher>(env: &mut HashMap<OsString, OsString, S>) {
    env.retain(|name, _| {
        let name = name.to_string_lossy().to_ascii_uppercase();
        !is_secret(&name)
    });
}

fn is_secret(name: &str) -> bool {
    if KEEP_EXACT.contains(&name) {
        return false;
    }
    SECRET_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || SECRET_MARKERS.iter().any(|marker| name.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn env(pairs: &[(&str, &str)]) -> HashMap<OsString, OsString> {
        pairs
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    #[test]
    fn credentials_are_removed_and_runtime_config_survives() {
        let mut env = env(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
            ("AWS_SECRET_ACCESS_KEY", "x"),
            ("OPENAI_API_KEY", "x"),
            ("GITHUB_TOKEN", "x"),
            ("MY_SERVICE_PASSWORD", "x"),
            ("GITHUB_CLIENT_ID", "public"),
            ("TOKENIZERS_PARALLELISM", "false"),
        ]);
        scrub_secrets(&mut env);
        let mut names: Vec<String> = env
            .keys()
            .map(|k| k.to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["GITHUB_CLIENT_ID", "HOME", "PATH", "TOKENIZERS_PARALLELISM"]
        );
    }

    #[test]
    fn ordinary_variables_are_untouched() {
        let mut env = env(&[("LANG", "en_US.UTF-8"), ("TERM", "xterm-256color")]);
        let before = env.clone();
        scrub_secrets(&mut env);
        assert_eq!(env, before);
    }
}
