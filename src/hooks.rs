use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// Run a pre-spin-up hook (spec §4.4): `$SHELL -lc`, login shell so the user's
/// normal PATH applies, in the invocation cwd, inheriting the caller's env.
///
/// stdout is protocol: each `KEY=VALUE` line becomes a param. stderr is human
/// output and passes through to the terminal. Non-zero exit aborts.
pub fn run_pre(cmd: &str, cwd: &Path) -> Result<Vec<(String, String)>> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let output = Command::new(shell).arg("-lc").arg(cmd).current_dir(cwd).output()?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(Error::HookFailed { code: output.status.code().unwrap_or(-1), stderr });
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
        if !stderr.ends_with('\n') {
            eprintln!();
        }
    }
    Ok(parse_kv_lines(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_kv_lines(stdout: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        match line.split_once('=') {
            Some((key, value)) if !key.is_empty() => pairs.push((key.to_string(), value.to_string())),
            _ => eprintln!("warning: ignoring malformed hook output line: {line:?}"),
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_value_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let pairs = run_pre("printf 'worktree=/tmp/wt\\nbranch=feat\\n'", dir.path()).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("worktree".to_string(), "/tmp/wt".to_string()),
                ("branch".to_string(), "feat".to_string())
            ]
        );
    }

    #[test]
    fn first_equals_splits_value_may_contain_equals() {
        let dir = tempfile::tempdir().unwrap();
        let pairs = run_pre("echo 'query=a=b=c'", dir.path()).unwrap();
        assert_eq!(pairs, vec![("query".to_string(), "a=b=c".to_string())]);
    }

    #[test]
    fn nonzero_exit_aborts_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_pre("echo 'something broke' >&2; exit 3", dir.path()).unwrap_err();
        match err {
            Error::HookFailed { code, stderr } => {
                assert_eq!(code, 3);
                assert!(stderr.contains("something broke"));
            }
            other => panic!("expected HookFailed, got {other:?}"),
        }
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let pairs = run_pre("printf 'ok=1\\nnot-a-kv-line\\n'", dir.path()).unwrap();
        assert_eq!(pairs, vec![("ok".to_string(), "1".to_string())]);
    }

    #[test]
    fn runs_in_given_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let pairs = run_pre("echo \"here=$PWD\"", dir.path()).unwrap();
        let got = pairs[0].1.clone();
        // /var vs /private/var on macOS — compare canonicalized.
        let want = dir.path().canonicalize().unwrap();
        assert_eq!(Path::new(&got).canonicalize().unwrap(), want);
    }
}
