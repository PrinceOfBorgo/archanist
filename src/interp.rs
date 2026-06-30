use anyhow::{Result, bail};
use std::collections::HashMap;

pub type Env = HashMap<String, String>;

/// Interpolate `{name}` placeholders against `env`. `{{` and `}}` are literal
/// braces. Errors on unterminated `{`, stray `}`, or references to variables
/// that aren't in `env`.
pub fn interpolate(template: &str, env: &Env) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
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
                    bail!("unterminated `{{` in template (started at '{{{}')", name);
                }
                let value = env
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("undefined variable '{{{}}}'", name))?;
                out.push_str(value);
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push('}');
                } else {
                    bail!("unexpected `}}` in template");
                }
            }
            _ => out.push(c),
        }
    }
    Ok(out)
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
        assert_eq!(interpolate("hello {name}", &e).unwrap(), "hello Alice");
    }

    #[test]
    fn multiple_substitutions() {
        let e = env(&[("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!(interpolate("{a}-{b}-{c}", &e).unwrap(), "1-2-3");
    }

    #[test]
    fn substitution_at_start_middle_end() {
        let e = env(&[("x", "X")]);
        assert_eq!(interpolate("{x}mid{x}end{x}", &e).unwrap(), "XmidXendX");
    }

    #[test]
    fn escaped_open_brace() {
        assert_eq!(
            interpolate("literal {{ brace", &env(&[])).unwrap(),
            "literal { brace"
        );
    }

    #[test]
    fn escaped_close_brace() {
        assert_eq!(
            interpolate("literal }} brace", &env(&[])).unwrap(),
            "literal } brace"
        );
    }

    #[test]
    fn escaped_and_substituted_mixed() {
        let e = env(&[("v", "3.2.1")]);
        assert_eq!(interpolate("{{ v = {v} }}", &e).unwrap(), "{ v = 3.2.1 }");
    }

    #[test]
    fn undefined_variable_errors() {
        let err = interpolate("hi {missing}", &env(&[])).unwrap_err();
        assert!(
            err.to_string().contains("missing"),
            "expected 'missing' in: {err}"
        );
    }

    #[test]
    fn unterminated_brace_errors() {
        let err = interpolate("hello {name", &env(&[("name", "x")])).unwrap_err();
        assert!(
            err.to_string().contains("unterminated"),
            "expected 'unterminated' in: {err}"
        );
    }

    #[test]
    fn stray_close_brace_errors() {
        let err = interpolate("hi } there", &env(&[])).unwrap_err();
        assert!(
            err.to_string().contains("unexpected"),
            "expected 'unexpected' in: {err}"
        );
    }
}
