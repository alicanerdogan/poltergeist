use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{Error, Result};

/// A declared workflow input (spec §4.3).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDecl {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// Resolved param values. Only set params are present; interpolation of an
/// absent param is an error, never a silent empty string.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params(pub BTreeMap<String, String>);

impl Params {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    /// Merge hook-emitted pairs. Hook output wins over flag/env/default
    /// (spec §4.4) and may introduce undeclared keys.
    pub fn merge_hook_output(&mut self, pairs: &[(String, String)]) {
        for (k, v) in pairs {
            self.0.insert(k.clone(), v.clone());
        }
    }
}

/// Resolve declared params with spec precedence: flag > environment > YAML
/// default > error if required. `env` is a lookup fn so tests stay pure.
pub fn resolve(
    decls: &BTreeMap<String, ParamDecl>,
    cli_params: &[(String, String)],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Params> {
    for (key, _) in cli_params {
        if !decls.contains_key(key) {
            return Err(Error::UndeclaredParam(key.clone()));
        }
    }
    let mut out = Params::default();
    for (name, decl) in decls {
        let value = cli_params
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| env(name))
            .or_else(|| decl.default.clone());
        match value {
            Some(v) => out.set(name.clone(), v),
            None if decl.required => return Err(Error::MissingParam(name.clone())),
            None => {}
        }
    }
    Ok(out)
}

/// Interpolate `${name}` references (spec §4.3). `$${` is an escaped literal
/// `${`. Unknown names and unterminated references are hard errors.
pub fn interpolate(template: &str, params: &Params) -> Result<String> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '$' && i + 1 < chars.len() && chars[i + 1] == '$' && i + 2 < chars.len() && chars[i + 2] == '{' {
            out.push_str("${");
            i += 3;
        } else if c == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
            let start = i + 2;
            let mut j = start;
            while j < chars.len() && chars[j] != '}' {
                j += 1;
            }
            if j >= chars.len() {
                return Err(Error::Message(format!(
                    "unterminated '${{' in template '{template}'"
                )));
            }
            let name: String = chars[start..j].iter().collect();
            match params.get(&name) {
                Some(v) => out.push_str(v),
                None => return Err(Error::UnresolvedVar(name)),
            }
            i = j + 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls() -> BTreeMap<String, ParamDecl> {
        BTreeMap::from([
            ("branch".into(), ParamDecl { required: true, default: None }),
            ("base".into(), ParamDecl { required: false, default: Some("main".into()) }),
            ("opt".into(), ParamDecl { required: false, default: None }),
        ])
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn flag_beats_env_beats_default() {
        let cli = vec![("branch".to_string(), "feat/login".to_string())];
        let env = |k: &str| (k == "base").then(|| "develop".to_string());
        let p = resolve(&decls(), &cli, &env).unwrap();
        assert_eq!(p.get("branch"), Some("feat/login"));
        assert_eq!(p.get("base"), Some("develop")); // env beats default
        assert_eq!(p.get("opt"), None); // optional, unset: absent

        let p = resolve(&decls(), &cli, &no_env).unwrap();
        assert_eq!(p.get("base"), Some("main")); // default
    }

    #[test]
    fn last_flag_wins_on_repeat() {
        let cli = vec![
            ("branch".to_string(), "a".to_string()),
            ("branch".to_string(), "b".to_string()),
        ];
        let p = resolve(&decls(), &cli, &no_env).unwrap();
        assert_eq!(p.get("branch"), Some("b"));
    }

    #[test]
    fn missing_required_errors() {
        let err = resolve(&decls(), &[], &no_env).unwrap_err();
        assert!(matches!(err, Error::MissingParam(n) if n == "branch"));
    }

    #[test]
    fn undeclared_flag_errors() {
        let cli = vec![("brnach".to_string(), "typo".to_string())];
        let err = resolve(&decls(), &cli, &no_env).unwrap_err();
        assert!(matches!(err, Error::UndeclaredParam(n) if n == "brnach"));
    }

    #[test]
    fn interpolate_basic_and_adjacent() {
        let p = Params(BTreeMap::from([("branch".into(), "feat".into()), ("base".into(), "main".into())]));
        assert_eq!(interpolate("review-${branch}", &p).unwrap(), "review-feat");
        assert_eq!(interpolate("${branch}${base}", &p).unwrap(), "featmain");
        assert_eq!(interpolate("no vars $here", &p).unwrap(), "no vars $here");
        assert_eq!(interpolate("trailing $", &p).unwrap(), "trailing $");
    }

    #[test]
    fn interpolate_escape() {
        let p = Params(BTreeMap::from([("branch".into(), "feat".into())]));
        assert_eq!(interpolate("$${branch} is ${branch}", &p).unwrap(), "${branch} is feat");
    }

    #[test]
    fn interpolate_unknown_var_errors() {
        let p = Params::default();
        let err = interpolate("hello ${world}", &p).unwrap_err();
        assert!(matches!(err, Error::UnresolvedVar(n) if n == "world"));
    }

    #[test]
    fn interpolate_unterminated_errors() {
        let p = Params::default();
        assert!(interpolate("oops ${branch", &p).is_err());
    }

    #[test]
    fn hook_output_overrides_and_adds_keys() {
        let mut p = Params(BTreeMap::from([("base".into(), "main".into())]));
        p.merge_hook_output(&[
            ("base".into(), "develop".into()),
            ("worktree".into(), "/tmp/wt".into()),
        ]);
        assert_eq!(p.get("base"), Some("develop"));
        assert_eq!(p.get("worktree"), Some("/tmp/wt"));
    }
}
