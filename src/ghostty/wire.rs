//! Delimited-text wire format for bridge I/O (TDD §4.1): fields separated by
//! ASCII 31 (unit separator), records by ASCII 30 (record separator) —
//! control characters that cannot collide with real titles or paths.

pub const US: char = '\x1f';
pub const RS: char = '\x1e';

/// Decode osascript output into records of fields. Tolerates the trailing
/// record separator and trailing newline osascript appends.
pub fn decode(output: &str) -> Vec<Vec<String>> {
    let s = output.strip_suffix('\n').unwrap_or(output);
    let s = s.strip_suffix(RS).unwrap_or(s);
    if s.is_empty() {
        return Vec::new();
    }
    s.split(RS)
        .filter(|rec| !rec.is_empty())
        .map(|rec| rec.split(US).map(str::to_string).collect())
        .collect()
}

/// Encode one record (test helper; the bridge never encodes user data into
/// script source — inputs travel as argv).
pub fn encode_record(fields: &[&str]) -> String {
    let mut out = fields.join(&US.to_string());
    out.push(RS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut out = encode_record(&["a", "b", "c"]);
        out.push_str(&encode_record(&["d", "e", "f"]));
        assert_eq!(decode(&out), vec![vec!["a", "b", "c"], vec!["d", "e", "f"]]);
    }

    #[test]
    fn tolerates_trailing_newline_and_missing_final_rs() {
        assert_eq!(decode("a\x1fb\n"), vec![vec!["a", "b"]]);
        assert_eq!(decode("a\x1fb"), vec![vec!["a", "b"]]);
    }

    #[test]
    fn empty_output() {
        assert!(decode("").is_empty());
        assert!(decode("\n").is_empty());
    }

    #[test]
    fn preserves_spaces_and_shell_metachars() {
        let out = encode_record(&["git commit -m 'a b'", "~/some dir"]);
        assert_eq!(decode(&out), vec![vec!["git commit -m 'a b'", "~/some dir"]]);
    }
}
