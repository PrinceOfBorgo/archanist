use anyhow::{Result, bail};
use std::collections::HashMap;

pub type Env = HashMap<String, String>;

/// Interpolate `${name}` placeholders against `env`. `$$` is a literal `$`.
/// Bare `{...}` (without a leading `$`) passes through untouched - step
/// implementations can use plain braces for their own step-local
/// placeholders (e.g. `db_migrate`'s `{file}` / `{name}` in the argv
/// template).
///
/// Names prefixed with `env.` are looked up in `env` first and then fall
/// back to the process environment (via [`std::env::var`]), so recipes
/// can reference OS environment variables as `${env.HOME}` etc.
///
/// Errors on unterminated `${` or references to variables missing from
/// both `env` and, for `env.*` names, the process environment.
pub fn interpolate(template: &str, env: &Env) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some(&'$') => {
                chars.next();
                out.push('$');
            }
            Some(&'{') => {
                chars.next(); // consume '{'
                let mut name = String::new();
                let mut closed = false;
                for nc in chars.by_ref() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    name.push(nc);
                }
                if !closed {
                    bail!("unterminated `${{` in template (started at '${{{}')", name);
                }
                let value = resolve(&name, env)?;
                out.push_str(&value);
            }
            _ => {
                // Lone '$' - emit literal
                out.push('$');
            }
        }
    }
    Ok(out)
}

/// Resolve a single `${name}` reference. `env.*` names fall back to the
/// process environment when absent from `env`.
fn resolve(name: &str, env: &Env) -> Result<String> {
    if let Some(v) = env.get(name) {
        return Ok(v.clone());
    }
    if let Some(os_name) = name.strip_prefix("env.") {
        return std::env::var(os_name)
            .map_err(|_| anyhow::anyhow!("undefined environment variable '${{env.{os_name}}}'"));
    }
    bail!("undefined variable '${{{}}}'", name)
}

/// Recursively interpolate every string value in a TOML value against `env`.
/// Table keys and non-string scalars pass through unchanged. Used to
/// pre-resolve a step config's body before the step is built, so `apply`
/// implementations see fully-resolved fields.
pub fn interpolate_toml(value: &toml::Value, env: &Env) -> Result<toml::Value> {
    match value {
        toml::Value::String(s) => interpolate(s, env).map(toml::Value::String),
        toml::Value::Array(arr) => arr
            .iter()
            .map(|v| interpolate_toml(v, env))
            .collect::<Result<Vec<_>>>()
            .map(toml::Value::Array),
        toml::Value::Table(t) => {
            let mut out = toml::Table::new();
            for (k, v) in t {
                out.insert(k.clone(), interpolate_toml(v, env)?);
            }
            Ok(toml::Value::Table(out))
        }
        other => Ok(other.clone()),
    }
}

/// Expand a component's `[vars]` table against a base environment.
///
/// Values may reference base keys (e.g. `{version}`) and, on the second
/// pass, other `vars.*` entries. Interpolation failures are tolerated:
/// an unresolvable reference leaves the raw template as-is. This lets
/// recipes declare vars in any order.
pub fn expand_component_vars(
    base: &Env,
    comp_vars: &std::collections::HashMap<String, String>,
) -> Env {
    let mut env = base.clone();
    // First pass: expand each var against the base env; missing refs to
    // sibling vars are left literal.
    for (k, v) in comp_vars {
        let expanded = interpolate(v, &env).unwrap_or_else(|_| v.clone());
        env.insert(format!("vars.{k}"), expanded);
    }
    // Second pass: now that all sibling vars are in `env` (at least in raw
    // form), re-expand vars whose value still contains unresolved refs.
    let keys: Vec<String> = comp_vars.keys().map(|k| format!("vars.{k}")).collect();
    for k in keys {
        if let Some(v) = env.get(&k).cloned() {
            let expanded = interpolate(&v, &env).unwrap_or(v);
            env.insert(k, expanded);
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn passthrough_when_no_placeholders() {
        assert_eq!(
            interpolate("hello world", &env(&[])).unwrap(),
            "hello world"
        );
    }

    #[test]
    fn empty_template() {
        assert_eq!(interpolate("", &env(&[])).unwrap(), "");
    }

    #[test]
    fn single_substitution() {
        let e = env(&[("name", "Alice")]);
        assert_eq!(interpolate("hello ${name}", &e).unwrap(), "hello Alice");
    }

    #[test]
    fn multiple_substitutions() {
        let e = env(&[("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!(interpolate("${a}-${b}-${c}", &e).unwrap(), "1-2-3");
    }

    #[test]
    fn substitution_at_start_middle_end() {
        let e = env(&[("x", "X")]);
        assert_eq!(interpolate("${x}mid${x}end${x}", &e).unwrap(), "XmidXendX");
    }

    #[test]
    fn plain_braces_pass_through_untouched() {
        // `{...}` without leading `$` is left alone - step-local placeholders
        // like db_migrate's `{file}` coexist with pipeline vars.
        assert_eq!(
            interpolate("psql -f {file}", &env(&[])).unwrap(),
            "psql -f {file}"
        );
    }

    #[test]
    fn escaped_dollar() {
        assert_eq!(interpolate("cost $$5", &env(&[])).unwrap(), "cost $5");
    }

    #[test]
    fn escaped_dollar_then_var() {
        let e = env(&[("amount", "42")]);
        // `$$` becomes literal `$`, then `{amount}` (no `$` prefix) passes through.
        assert_eq!(
            interpolate("$${amount} of ${amount}", &e).unwrap(),
            "${amount} of 42"
        );
    }

    #[test]
    fn lone_dollar_stays_literal() {
        assert_eq!(
            interpolate("price: $ 12", &env(&[])).unwrap(),
            "price: $ 12"
        );
    }

    #[test]
    fn mixed_var_and_step_local_braces() {
        let e = env(&[("version", "3.2.1")]);
        assert_eq!(
            interpolate("psql -h db -v v=${version} -f {file}", &e).unwrap(),
            "psql -h db -v v=3.2.1 -f {file}"
        );
    }

    #[test]
    fn undefined_variable_errors() {
        let err = interpolate("hi ${missing}", &env(&[])).unwrap_err();
        assert!(
            err.to_string().contains("missing"),
            "expected 'missing' in: {err}"
        );
    }

    #[test]
    fn env_prefix_reads_from_process_environment() {
        // Use a name unlikely to collide with anything already set.
        let key = "ARCHANIST_INTERP_TEST_ENV_VAR";
        // SAFETY: the value is process-global; the test is single-threaded
        // within this module and no other test touches this key.
        unsafe { std::env::set_var(key, "from-os") };
        let got = interpolate(&format!("v=${{env.{key}}}"), &env(&[])).unwrap();
        unsafe { std::env::remove_var(key) };
        assert_eq!(got, "v=from-os");
    }

    #[test]
    fn env_prefix_prefers_pipeline_env_over_process() {
        let key = "ARCHANIST_INTERP_TEST_SHADOW";
        unsafe { std::env::set_var(key, "os-value") };
        let e = env(&[(&format!("env.{key}"), "pipeline-value")]);
        let got = interpolate(&format!("v=${{env.{key}}}"), &e).unwrap();
        unsafe { std::env::remove_var(key) };
        assert_eq!(got, "v=pipeline-value");
    }

    #[test]
    fn env_prefix_missing_in_process_errors() {
        let key = "ARCHANIST_INTERP_TEST_MISSING";
        // Make sure it's really absent.
        unsafe { std::env::remove_var(key) };
        let err = interpolate(&format!("${{env.{key}}}"), &env(&[])).unwrap_err();
        assert!(err.to_string().contains(key), "expected '{key}' in: {err}");
    }

    #[test]
    fn unterminated_dollar_brace_errors() {
        let err = interpolate("hello ${name", &env(&[("name", "x")])).unwrap_err();
        assert!(
            err.to_string().contains("unterminated"),
            "expected 'unterminated' in: {err}"
        );
    }

    fn map(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn expand_vars_uses_base_env() {
        let base = env(&[("version", "1.2.3")]);
        let vars = map(&[("dest", "/tmp/app-${version}.zip")]);
        let out = expand_component_vars(&base, &vars);
        assert_eq!(
            out.get("vars.dest").map(String::as_str),
            Some("/tmp/app-1.2.3.zip")
        );
    }

    #[test]
    fn expand_vars_second_pass_resolves_sibling_refs() {
        let base = env(&[("version", "1.2.3")]);
        // `dest` references `${vars.stage}`; iteration order may visit `dest` first.
        let vars = map(&[
            ("stage", "/staging/v${version}"),
            ("dest", "${vars.stage}/bundle.zip"),
        ]);
        let out = expand_component_vars(&base, &vars);
        assert_eq!(
            out.get("vars.stage").map(String::as_str),
            Some("/staging/v1.2.3")
        );
        assert_eq!(
            out.get("vars.dest").map(String::as_str),
            Some("/staging/v1.2.3/bundle.zip")
        );
    }

    #[test]
    fn expand_vars_leaves_unresolvable_as_literal() {
        // No `unknown` in base or vars - the ref stays literal after both passes.
        let base = env(&[]);
        let vars = map(&[("dest", "prefix-${unknown}")]);
        let out = expand_component_vars(&base, &vars);
        assert_eq!(
            out.get("vars.dest").map(String::as_str),
            Some("prefix-${unknown}")
        );
    }

    #[test]
    fn interpolate_toml_recurses_into_tables_and_arrays() {
        let e = env(&[("version", "1.2.3"), ("dest", "/tmp")]);
        let v: toml::Value = toml::from_str(
            r#"
            url = "https://example.com/v${version}"
            paths = ["${dest}/a", "${dest}/b"]

            [nested]
            command = ["psql", "-h", "db", "-f", "{file}"]
            "#,
        )
        .unwrap();
        let out = interpolate_toml(&v, &e).unwrap();
        assert_eq!(
            out.get("url").and_then(|v| v.as_str()),
            Some("https://example.com/v1.2.3")
        );
        let paths = out.get("paths").and_then(|v| v.as_array()).unwrap();
        assert_eq!(paths[0].as_str(), Some("/tmp/a"));
        assert_eq!(paths[1].as_str(), Some("/tmp/b"));
        // Plain braces in nested `command` array stay literal.
        let cmd = out
            .get("nested")
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(cmd[4].as_str(), Some("{file}"));
    }

    #[test]
    fn interpolate_toml_preserves_non_strings() {
        let e = env(&[]);
        let v: toml::Value = toml::from_str("port = 8080\nverbose = true\n").unwrap();
        let out = interpolate_toml(&v, &e).unwrap();
        assert_eq!(out.get("port").and_then(|v| v.as_integer()), Some(8080));
        assert_eq!(out.get("verbose").and_then(|v| v.as_bool()), Some(true));
    }
}
