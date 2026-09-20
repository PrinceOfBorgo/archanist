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
use semver::Version;
use serde::Deserialize;
use std::collections::HashMap;
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
    /// Optional per-line semver filter applied after section slicing and
    /// before `strip` / extraction. Keeps only lines whose captured
    /// version is strictly newer than a reference version.
    #[serde(default)]
    pub version_filter: Option<VersionFilter>,
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

/// Optional per-line semver filter.
///
/// Every line matching `pattern` and capturing a semver in group `group`
/// is KEPT only when that version is strictly newer than `newer_than`.
/// Lines that don't match, or whose captured value isn't valid semver,
/// pass through unchanged. When `newer_than` itself isn't valid semver
/// (e.g. the `(none)` sentinel on a fresh install), the filter is a no-op
/// and every line is kept - so a first install still sees every row.
#[derive(Debug, Deserialize)]
pub struct VersionFilter {
    /// Multiline regex with a named capture group holding the version.
    pub pattern: String,
    /// Reference version; only lines strictly newer than this survive.
    pub newer_than: String,
    /// Name of the capture group holding the version. Defaults to `version`.
    #[serde(default = "default_version_group")]
    pub group: String,
}

fn default_version_group() -> String {
    "version".to_string()
}

/// One extraction rule. The pattern must declare one or more named
/// capture groups (`(?<name>...)`); each becomes an exported variable.
#[derive(Debug, Deserialize)]
pub struct Extract {
    /// Regex applied with `captures_iter` to the (possibly stripped and
    /// sectioned) text. Must contain at least one named capture group.
    pub pattern: String,
    /// Optional prefix prepended to every exported variable name. For
    /// example, `prefix = "vars."` with a named group `migrations`
    /// publishes `vars.migrations`, reachable via `${vars.migrations}`
    /// in later steps.
    #[serde(default)]
    pub prefix: String,
    /// Remove duplicates per exported variable before joining.
    #[serde(default)]
    pub dedupe: bool,
    /// Lexicographically sort per exported variable before joining.
    #[serde(default)]
    pub sort: bool,
    /// Separator placed between collected items. Default newline.
    #[serde(default = "default_join")]
    pub join: String,
    /// If the pattern produces zero matches, fail the step instead of
    /// publishing empty strings.
    #[serde(default)]
    pub required: bool,
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

fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Keep only lines whose captured version is strictly newer than
/// `filter.newer_than`. See [`VersionFilter`] for the full contract.
fn apply_version_filter(text: &str, filter: &VersionFilter) -> Result<String> {
    // A non-semver reference (e.g. the `(none)` fresh-install sentinel)
    // disables filtering: every row is kept so a first install runs all.
    let Ok(reference) = Version::parse(strip_v(filter.newer_than.trim())) else {
        return Ok(text.to_string());
    };
    let re = build_multiline(&filter.pattern, "version_filter.pattern")?;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let keep = match re.captures(line).and_then(|c| c.name(&filter.group)) {
            Some(m) => match Version::parse(strip_v(m.as_str().trim())) {
                Ok(v) => v > reference,
                // Unparsable version cell: keep rather than silently drop.
                Err(_) => true,
            },
            // No version on this line (header, separator, prose): keep.
            None => true,
        };
        if keep {
            out.push_str(line);
        }
    }
    Ok(out)
}

fn run_extract(text: &str, rule: &Extract) -> Result<Vec<(String, String)>> {
    let re = Regex::new(&rule.pattern)
        .with_context(|| format!("invalid extract.pattern regex: {}", rule.pattern))?;

    // Discover the named groups the pattern declares, in declaration order,
    // skipping the implicit whole-match group at index 0 and any unnamed
    // sub-groups the user included.
    let names: Vec<String> = re
        .capture_names()
        .flatten()
        .map(|s| s.to_string())
        .collect();
    if names.is_empty() {
        bail!(
            "extract pattern must contain at least one named capture group `(?<name>...)`: {}",
            rule.pattern
        );
    }

    // Collect the per-name value list across every match.
    let mut lists: HashMap<String, Vec<String>> =
        names.iter().map(|n| (n.clone(), Vec::new())).collect();
    for caps in re.captures_iter(text) {
        for name in &names {
            if let Some(m) = caps.name(name) {
                lists.get_mut(name).unwrap().push(m.as_str().to_string());
            }
        }
    }

    if rule.required && lists.values().any(|v| v.is_empty()) {
        let empty: Vec<&str> = names
            .iter()
            .filter(|n| lists.get(*n).map(|v| v.is_empty()).unwrap_or(true))
            .map(String::as_str)
            .collect();
        bail!(
            "extract produced zero matches for named group(s) {:?} (pattern: {})",
            empty,
            rule.pattern
        );
    }

    // Post-process each group independently, then publish under
    // `<prefix><group_name>`.
    let mut out: Vec<(String, String)> = Vec::with_capacity(names.len());
    for name in &names {
        let mut items = lists.remove(name).unwrap_or_default();
        if rule.dedupe {
            let mut seen = std::collections::HashSet::new();
            items.retain(|s| seen.insert(s.clone()));
        }
        if rule.sort {
            items.sort();
        }
        let value = items.join(&rule.join);
        out.push((format!("{}{}", rule.prefix, name), value));
    }
    Ok(out)
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
            let filtered = match &self.version_filter {
                Some(vf) => apply_version_filter(&scoped, vf)
                    .with_context(|| format!("step '{}': version filter failed", id))?,
                None => scoped,
            };
            let cleaned = apply_strip(&filtered, &self.strip)
                .with_context(|| format!("step '{}': strip failed", id))?;

            let mut exported = Env::new();
            for rule in &self.extract {
                let outputs = run_extract(&cleaned, rule).with_context(|| {
                    format!(
                        "step '{}': extract for pattern '{}' failed",
                        id, rule.pattern
                    )
                })?;
                for (name, value) in outputs {
                    let count = if value.is_empty() {
                        0
                    } else {
                        value.split(&rule.join).filter(|s| !s.is_empty()).count()
                    };
                    debug!("[{}] exported {} = {:?}", id, name, value);
                    info!("[{}] exported `{}` = {} item(s)", id, name, count);
                    exported.insert(name, value);
                }
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

| Version | Migrations                                 | Notes   |
| ------- | -------------------------------------------| ------- |
| v0.2.0  | `001_init.sql`                             | init    |
| v0.2.3  | ~~`002_add_ts.sql`~~, `003_create_XYZ.sql` |         |
| v0.2.4  | `002_add_ts.sql`, `004_overwrite_X.sql`    | fix 002 |

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
            pattern = "`(?<items>[^`]+)`"
            prefix  = "vars."
            dedupe  = true
            sort    = true
            "#,
        ))
        .unwrap();
        assert_eq!(step.extract.len(), 1);
        assert_eq!(step.extract[0].prefix, "vars.");
        assert!(step.extract[0].dedupe);
        assert!(step.extract[0].sort);
    }

    #[test]
    fn extract_defaults_prefix_to_empty_and_join_to_newline() {
        let step = ParseTextStep::from_body(body(
            r#"
            file = "/tmp/x"
            [[extract]]
            pattern = "(?<x>x)"
            "#,
        ))
        .unwrap();
        assert_eq!(step.extract[0].prefix, "");
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

    fn vfilter(newer_than: &str) -> VersionFilter {
        VersionFilter {
            pattern: r"^\|\s*v(?<version>\d+\.\d+\.\d+)\s*\|".into(),
            newer_than: newer_than.into(),
            group: "version".into(),
        }
    }

    const TABLE: &str = "| Version | Migrations |\n| ------- | ---------- |\n| v0.2.0  | `001` |\n| v0.3.2  | `009` |\n| v0.4.2  | `010` |\n";

    #[test]
    fn version_filter_keeps_only_newer_rows() {
        let out = apply_version_filter(TABLE, &vfilter("0.3.2")).unwrap();
        assert!(!out.contains("001"));
        assert!(!out.contains("009"));
        assert!(out.contains("010"));
        // Header and separator (no version) are preserved.
        assert!(out.contains("| Version | Migrations |"));
    }

    #[test]
    fn version_filter_drops_all_when_current_is_newest() {
        let out = apply_version_filter(TABLE, &vfilter("0.4.2")).unwrap();
        assert!(!out.contains("001"));
        assert!(!out.contains("009"));
        assert!(!out.contains("010"));
    }

    #[test]
    fn version_filter_rowless_current_drops_older_rows() {
        // v0.4.0 has no row. Older migrations (001, 009) are dropped, but a
        // newer row (v0.4.2 -> 010) is still correctly selected.
        let out = apply_version_filter(TABLE, &vfilter("0.4.0")).unwrap();
        assert!(!out.contains("001"));
        assert!(!out.contains("009"));
        assert!(out.contains("010"));
    }

    #[test]
    fn version_filter_none_sentinel_keeps_everything() {
        let out = apply_version_filter(TABLE, &vfilter("(none)")).unwrap();
        assert_eq!(out, TABLE);
    }

    #[test]
    fn version_filter_tolerates_v_prefixed_reference() {
        let out = apply_version_filter(TABLE, &vfilter("v0.3.2")).unwrap();
        assert!(out.contains("010"));
        assert!(!out.contains("009"));
    }

    fn extract_rule(pattern: &str, prefix: &str) -> Extract {
        Extract {
            pattern: pattern.into(),
            prefix: prefix.into(),
            dedupe: false,
            sort: false,
            join: ",".into(),
            required: false,
        }
    }

    #[test]
    fn extract_rejects_pattern_without_named_groups() {
        let err =
            run_extract("one two three", &extract_rule("(\\w+)", "")).expect_err("expected error");
        assert!(err.to_string().contains("named capture"), "err: {err}");
    }

    #[test]
    fn extract_collects_named_group_across_matches() {
        let out = run_extract(
            "one=1\ntwo=2\nthree=3\n",
            &Extract {
                pattern: "(?m)^(?<key>\\w+)=(?<val>\\S+)$".into(),
                prefix: "vars.".into(),
                dedupe: false,
                sort: false,
                join: ",".into(),
                required: false,
            },
        )
        .unwrap();
        // Two named groups, each collected across all three matches.
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        assert_eq!(
            map.get("vars.key").map(String::as_str),
            Some("one,two,three")
        );
        assert_eq!(map.get("vars.val").map(String::as_str), Some("1,2,3"));
    }

    #[test]
    fn extract_dedupe_and_sort_apply_per_group() {
        let out = run_extract(
            "a b a c b",
            &Extract {
                pattern: "(?<letter>[abc])".into(),
                prefix: "vars.".into(),
                dedupe: true,
                sort: true,
                join: ",".into(),
                required: false,
            },
        )
        .unwrap();
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        assert_eq!(map.get("vars.letter").map(String::as_str), Some("a,b,c"));
    }

    #[test]
    fn extract_required_fails_on_zero_matches() {
        let err = run_extract(
            "no digits here",
            &Extract {
                pattern: "(?<n>\\d+)".into(),
                prefix: "vars.".into(),
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
    fn extract_without_prefix_publishes_bare_group_name() {
        let out = run_extract(
            "v1.2.3",
            &Extract {
                pattern: "^v(?<major>\\d+)\\.(?<minor>\\d+)\\.(?<patch>\\d+)$".into(),
                prefix: "".into(),
                dedupe: false,
                sort: false,
                join: ",".into(),
                required: true,
            },
        )
        .unwrap();
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        assert_eq!(map.get("major").map(String::as_str), Some("1"));
        assert_eq!(map.get("minor").map(String::as_str), Some("2"));
        assert_eq!(map.get("patch").map(String::as_str), Some("3"));
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
                pattern: "`(?<migrations>[^`]+\\.sql)`".into(),
                prefix: "vars.".into(),
                dedupe: true,
                sort: true,
                join: "\n".into(),
                required: true,
            },
        )
        .unwrap();
        let map: std::collections::HashMap<_, _> = out.into_iter().collect();
        let lines: Vec<&str> = map.get("vars.migrations").unwrap().split('\n').collect();
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
