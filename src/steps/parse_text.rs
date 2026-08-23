//! `parse_text`: read a text file, extract data with regex, and publish
//! the results as interpolation variables for subsequent pipeline steps.
//!
//! Design goals:
//! * Driver-agnostic - no baked-in knowledge of any particular document
//!   format (markdown, INI, whatever).
//! * Composable - multiple `[[extract]]` rules can run against the same
//!   input, each publishing under its own variable name.
//! * Side-effect-free - only reads a file and writes vars; nothing on disk.
//!
//! # Example: pull migration filenames out of a markdown table section
//!
//! ```toml
//! [[steps]]
//! id      = "select_migrations"
//! type    = "parse_text"
//! file    = "${vars.bundle_extracted_in}/MIGRATIONS.md"
//! # Restrict matching to a slice of the file. Both bounds are regex.
//! # `start` matches first; everything before it is dropped.
//! # `end` is then matched in the remainder; everything from `end` onward
//! # is dropped.
//! section = { start = "## Migration Reference", end = "^## " }
//! # Regexes whose matches are deleted from the working text BEFORE the
//! # extract rules run. Useful for skipping strikethroughs, comments, etc.
//! strip   = ["~~`[^`]+`~~"]
//!
//! [[steps.extract]]
//! # Capture group 1 of every match is collected.
//! pattern = "`([^`]+\\.sql)`"
//! # Where to publish. Keys are exposed verbatim; include the `vars.`
//! # prefix if you want `${vars.X}` to resolve.
//! into    = "vars.migrations"
//! dedupe  = true
//! sort    = true
//! # Joined into a single string with this separator before being stored.
//! join    = "\n"
//! ```

use crate::interp::Env;
use crate::steps::{BoxFuture, Step, StepCtx, StepOutcome};
use anyhow::{Context, Result, bail};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info};

#[derive(Debug, Deserialize)]
pub struct ParseTextStep {
    /// Path to the input file. Relative paths resolve against
    /// [`StepCtx::base_dir`].
    pub file: PathBuf,
    /// Optional region bounds. Every field is an independent regex,
    /// compiled with multiline mode so `^` / `$` match line boundaries.
    #[serde(default)]
    pub section: Option<Section>,
    /// Regexes whose matches are removed from the working text before
    /// any [`Extract`] rule runs.
    #[serde(default)]
    pub strip: Vec<String>,
    /// Extraction rules. At least one is required.
    #[serde(default)]
    pub extract: Vec<Extract>,
}

/// Optional bounds that clip the working text before extraction runs.
#[derive(Debug, Deserialize)]
pub struct Section {
    /// Drop everything before the FIRST match of this regex (the match
    /// itself is kept). Errors if the regex doesn't match.
    #[serde(default)]
    pub start: Option<String>,
    /// Like `start`, but the cursor is moved PAST the match (the match
    /// is excluded from the scoped slice). Silently no-ops when the
    /// regex doesn't match - useful for "skip everything up to and
    /// including the row for `${current_version}`, which may be empty
    /// on first install".
    #[serde(default)]
    pub start_after: Option<String>,
    /// In the remaining text, drop everything from the FIRST match of
    /// this regex onward (the match itself is dropped). Searched
    /// starting AFTER the first line so `start` and `end` can both be
    /// `^## ` without `end` matching the section heading itself.
    #[serde(default)]
    pub end: Option<String>,
}

/// One extraction rule: pattern, target var, and post-processing.
#[derive(Debug, Deserialize)]
pub struct Extract {
    /// Regex applied with `captures_iter` to the (possibly stripped and
    /// sectioned) text.
    pub pattern: String,
    /// Capture group to collect from each match. Default `1`. Use `0`
    /// for the whole match.
    #[serde(default = "default_group")]
    pub group: usize,
    /// Variable name under which the result is published. Include the
    /// `vars.` prefix to be reachable via `${vars.X}` in later steps.
    pub into: String,
    /// Remove duplicates from the collected list before joining.
    #[serde(default)]
    pub dedupe: bool,
    /// Lexicographically sort the collected list before joining.
    #[serde(default)]
    pub sort: bool,
    /// Separator placed between collected items. Default newline.
    #[serde(default = "default_join")]
    pub join: String,
    /// If the pattern produces zero matches, fail the step instead of
    /// publishing an empty string.
    #[serde(default)]
    pub required: bool,
}

fn default_group() -> usize {
    1
}

fn default_join() -> String {
    "\n".to_string()
}

impl ParseTextStep {
    pub fn from_body(body: toml::Value) -> Result<Self> {
        let step: ParseTextStep = body.try_into().context("invalid parse_text config")?;
        if step.extract.is_empty() {
            bail!("at least one `[[extract]]` rule is required");
        }
        Ok(step)
    }
}

fn build_multiline(pattern: &str, label: &str) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .multi_line(true)
        .build()
        .with_context(|| format!("invalid {label} regex: {pattern}"))
}

fn apply_section(text: &str, section: &Section) -> Result<String> {
    let mut slice = text.to_string();
    if let Some(start) = &section.start {
        let re = build_multiline(start, "section.start")?;
        match re.find(&slice) {
            Some(m) => slice = slice[m.start()..].to_string(),
            None => bail!("section.start regex did not match: {start}"),
        }
    }
    if let Some(start_after) = &section.start_after {
        let re = build_multiline(start_after, "section.start_after")?;
        if let Some(m) = re.find(&slice) {
            slice = slice[m.end()..].to_string();
        }
        // No match: leave the slice unchanged.
    }
    if let Some(end) = &section.end {
        let re = build_multiline(end, "section.end")?;
        // Search AFTER the first line so `start` and `end` can both be
        // `^## ` without `end` matching the section heading itself.
        let search_from = slice.find('\n').map(|i| i + 1).unwrap_or(slice.len());
        if let Some(m) = re.find_at(&slice, search_from) {
            slice.truncate(m.start());
        }
    }
    Ok(slice)
}

fn apply_strip(text: &str, patterns: &[String]) -> Result<String> {
    let mut out = text.to_string();
    for p in patterns {
        let re = Regex::new(p).with_context(|| format!("invalid strip regex: {p}"))?;
        out = re.replace_all(&out, "").into_owned();
    }
    Ok(out)
}

fn run_extract(text: &str, rule: &Extract) -> Result<String> {
    let re = Regex::new(&rule.pattern)
        .with_context(|| format!("invalid extract.pattern regex: {}", rule.pattern))?;
    let mut items: Vec<String> = Vec::new();
    for caps in re.captures_iter(text) {
        if let Some(m) = caps.get(rule.group) {
            items.push(m.as_str().to_string());
        }
    }
    if items.is_empty() && rule.required {
        bail!(
            "extract '{}' produced zero matches (pattern: {})",
            rule.into,
            rule.pattern
        );
    }
    if rule.dedupe {
        let mut seen = std::collections::HashSet::new();
        items.retain(|s| seen.insert(s.clone()));
    }
    if rule.sort {
        items.sort();
    }
    Ok(items.join(&rule.join))
}

impl Step for ParseTextStep {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let path = if self.file.is_absolute() {
                self.file.clone()
            } else {
                ctx.base_dir.join(&self.file)
            };
            info!("[{}] scanning {}", id, path.display());
            let raw = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("step '{}': failed to read {}", id, path.display()))?;

            let scoped = match &self.section {
                Some(s) => apply_section(&raw, s)
                    .with_context(|| format!("step '{}': section slicing failed", id))?,
                None => raw,
            };
            let cleaned = apply_strip(&scoped, &self.strip)
                .with_context(|| format!("step '{}': strip failed", id))?;

            let mut exported = Env::new();
            for rule in &self.extract {
                let value = run_extract(&cleaned, rule)
                    .with_context(|| format!("step '{}': extract '{}' failed", id, rule.into))?;
                let count = if value.is_empty() {
                    0
                } else {
                    value.split(&rule.join).filter(|s| !s.is_empty()).count()
                };
                debug!("[{}] exported {} = {:?}", id, rule.into, value);
                info!("[{}] exported `{}` = {} item(s)", id, rule.into, count);
                exported.insert(rule.into.clone(), value);
            }

            Ok(StepOutcome {
                exported_vars: exported,
                ..Default::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = r#"# Header

Intro.

## Migration Reference

The following table lists migrations.

| Version | Migrations                                              | Notes |
| ------- | ------------------------------------------------------- | ----- |
| v0.2.0  | `001_init.sql`                                          | init  |
| v0.2.3  | ~~`002_add_ts.sql`~~, `003_create_XYZ.sql`              |       |
| v0.2.4  | `002_add_ts.sql`, `004_overwrite_X.sql`                 |       |

## Next Section

Should be excluded.

`999_unrelated.sql`
"#;

    fn body(text: &str) -> toml::Value {
        toml::Value::Table(toml::from_str(text).unwrap())
    }

    #[test]
    fn from_body_requires_at_least_one_extract() {
        let err = ParseTextStep::from_body(body(r#"file = "/tmp/x""#)).expect_err("expected error");
        assert!(err.to_string().contains("`[[extract]]`"), "err: {err}");
    }

    #[test]
    fn from_body_accepts_full_shape() {
        let step = ParseTextStep::from_body(body(
            r#"
            file = "/tmp/x"
            strip = ["foo"]
            section = { start = "^A", end = "^B" }

            [[extract]]
            pattern = "`([^`]+)`"
            into    = "vars.items"
            dedupe  = true
            sort    = true
            "#,
        ))
        .unwrap();
        assert_eq!(step.extract.len(), 1);
        assert_eq!(step.extract[0].into, "vars.items");
        assert!(step.extract[0].dedupe);
        assert!(step.extract[0].sort);
    }

    #[test]
    fn extract_defaults_group_to_1_and_join_to_newline() {
        let step = ParseTextStep::from_body(body(
            r#"
            file = "/tmp/x"
            [[extract]]
            pattern = "x"
            into    = "vars.x"
            "#,
        ))
        .unwrap();
        assert_eq!(step.extract[0].group, 1);
        assert_eq!(step.extract[0].join, "\n");
    }

    #[test]
    fn section_bounds_clip_correctly() {
        let s = apply_section(
            MD,
            &Section {
                start: Some("## Migration Reference".into()),
                start_after: None,
                end: Some("^## ".into()),
            },
        )
        .unwrap();
        assert!(s.contains("001_init.sql"));
        assert!(s.contains("004_overwrite_X.sql"));
        assert!(!s.contains("999_unrelated.sql"));
        assert!(!s.contains("Next Section"));
    }

    #[test]
    fn section_start_after_skips_the_match_itself() {
        let text = "before\nMARKER\nafter\n";
        let s = apply_section(
            text,
            &Section {
                start: None,
                start_after: Some("MARKER".into()),
                end: None,
            },
        )
        .unwrap();
        assert_eq!(s, "\nafter\n");
    }

    #[test]
    fn section_start_after_is_optional_noop_when_no_match() {
        let text = "hello\nworld\n";
        let s = apply_section(
            text,
            &Section {
                start: None,
                start_after: Some("nothing_here".into()),
                end: None,
            },
        )
        .unwrap();
        assert_eq!(s, text);
    }

    #[test]
    fn section_start_errors_when_no_match() {
        let err = apply_section(
            "no marker anywhere",
            &Section {
                start: Some("MARKER".into()),
                start_after: None,
                end: None,
            },
        )
        .expect_err("expected error");
        assert!(err.to_string().contains("section.start"), "err: {err}");
    }

    #[test]
    fn strip_removes_matched_spans() {
        let out = apply_strip("hi ~~`nope`~~ ok `keep`", &["~~`[^`]+`~~".to_string()]).unwrap();
        assert_eq!(out, "hi  ok `keep`");
    }

    #[test]
    fn extract_collects_capture_group_1_by_default() {
        let out = run_extract(
            "one=1\ntwo=2\nthree=3\n",
            &Extract {
                pattern: "^(\\w+)=".into(),
                group: 1,
                into: "vars.keys".into(),
                dedupe: false,
                sort: false,
                join: ",".into(),
                required: false,
            },
        )
        .unwrap();
        // Non-multiline default: pattern only matches the first line.
        assert_eq!(out, "one");
    }

    #[test]
    fn extract_dedupe_and_sort() {
        let out = run_extract(
            "a b a c b",
            &Extract {
                pattern: "([abc])".into(),
                group: 1,
                into: "vars.letters".into(),
                dedupe: true,
                sort: true,
                join: ",".into(),
                required: false,
            },
        )
        .unwrap();
        assert_eq!(out, "a,b,c");
    }

    #[test]
    fn extract_required_fails_on_zero_matches() {
        let err = run_extract(
            "no digits here",
            &Extract {
                pattern: "(\\d+)".into(),
                group: 1,
                into: "vars.n".into(),
                dedupe: false,
                sort: false,
                join: ",".into(),
                required: true,
            },
        )
        .expect_err("expected error");
        assert!(err.to_string().contains("zero matches"), "err: {err}");
    }

    #[test]
    fn extract_group_0_returns_whole_match() {
        let out = run_extract(
            "abc123def",
            &Extract {
                pattern: "\\d+".into(),
                group: 0,
                into: "vars.n".into(),
                dedupe: false,
                sort: false,
                join: ",".into(),
                required: false,
            },
        )
        .unwrap();
        assert_eq!(out, "123");
    }

    #[test]
    fn end_to_end_extracts_migrations_from_markdown_table() {
        let scoped = apply_section(
            MD,
            &Section {
                start: Some("## Migration Reference".into()),
                start_after: None,
                end: Some("^## ".into()),
            },
        )
        .unwrap();
        let cleaned = apply_strip(&scoped, &["~~`[^`]+`~~".to_string()]).unwrap();
        let out = run_extract(
            &cleaned,
            &Extract {
                pattern: "`([^`]+\\.sql)`".into(),
                group: 1,
                into: "vars.migrations".into(),
                dedupe: true,
                sort: true,
                join: "\n".into(),
                required: true,
            },
        )
        .unwrap();
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(
            lines,
            vec![
                "001_init.sql",
                "002_add_ts.sql",
                "003_create_XYZ.sql",
                "004_overwrite_X.sql",
            ]
        );
    }
}
