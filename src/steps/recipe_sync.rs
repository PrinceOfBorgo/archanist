//! `recipe_sync`: install component recipes into the components dir.
//!
//! `src` is a recipe file or a directory of `*.toml` recipes (not
//! recursive); each lands under its own file name. Every recipe is parsed
//! and its step types checked against this binary before anything is
//! written, so a recipe that would break the next run never reaches disk.
//! Files are written atomically and, with `backup = true` (the default),
//! snapshotted for `archanist rollback`.
//!
//! With `reload = true` the rest of the run continues with the running
//! component's new recipe; otherwise the changes apply from the next run.

use crate::config::{ArchanistConfig, ComponentConfig};
use crate::steps::backup::{self, BackupBuilder};
use crate::steps::{BoxFuture, Step, StepCtx, StepKind, StepOutcome, builtin_registry};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::info;

pub struct RecipeSync {
    src: String,
    reload: bool,
    backup: bool,
}

#[derive(Deserialize)]
struct RecipeSyncRaw {
    src: String,
    #[serde(default)]
    reload: bool,
    #[serde(default = "default_backup")]
    backup: bool,
}

fn default_backup() -> bool {
    true
}

impl StepKind for RecipeSync {
    const KIND: &'static str = "recipe_sync";

    fn from_body(body: toml::Value) -> Result<Self> {
        let raw: RecipeSyncRaw = body.try_into().context("invalid recipe_sync config")?;
        Ok(Self {
            src: raw.src,
            reload: raw.reload,
            backup: raw.backup,
        })
    }
}

fn check_kinds(comp: &ComponentConfig, known: &[&str]) -> Result<()> {
    let setup = comp.sandbox.iter().flat_map(|sb| &sb.setup);
    for step in comp.steps.iter().chain(setup) {
        if !known.contains(&step.kind.as_str()) {
            bail!(
                "step '{}' has type '{}', which this archanist doesn't support",
                step.id,
                step.kind
            );
        }
    }
    Ok(())
}

impl Step for RecipeSync {
    fn apply<'a>(&'a self, ctx: &'a StepCtx) -> BoxFuture<'a, Result<StepOutcome>> {
        Box::pin(async move {
            let id = &ctx.step_id;
            let src = Path::new(&self.src);
            let files = recipe_files(src)
                .await
                .with_context(|| format!("step '{}': can't read {}", id, src.display()))?;
            if files.is_empty() {
                bail!("step '{}': no *.toml recipes in {}", id, src.display());
            }

            // Validate everything before writing anything.
            let registry = builtin_registry();
            let known = registry.kinds();
            let own_file = format!("{}.toml", ctx.component);
            let mut recipes = Vec::with_capacity(files.len());
            for file in files {
                let content = tokio::fs::read_to_string(&file)
                    .await
                    .with_context(|| format!("step '{}': can't read {}", id, file.display()))?;
                let comp = ArchanistConfig::parse_component(&content, &file)
                    .with_context(|| format!("step '{}': invalid recipe", id))?;
                check_kinds(&comp, &known)
                    .with_context(|| format!("step '{}': invalid recipe {}", id, file.display()))?;
                let name = file
                    .file_name()
                    .with_context(|| format!("step '{}': bad path {}", id, file.display()))?
                    .to_owned();
                if self.reload
                    && name == own_file.as_str()
                    && !comp.steps.iter().any(|s| &s.id == id)
                {
                    bail!(
                        "step '{}': reload = true, but the new {} has no step '{}' to continue from",
                        id,
                        file.display(),
                        id
                    );
                }
                recipes.push((name, content));
            }

            tokio::fs::create_dir_all(&ctx.components_dir)
                .await
                .with_context(|| {
                    format!(
                        "step '{}': failed to create {}",
                        id,
                        ctx.components_dir.display()
                    )
                })?;
            let mut builder = if self.backup {
                Some(BackupBuilder::new(backup::reset(ctx).await?))
            } else {
                None
            };
            for (name, content) in &recipes {
                let dest = ctx.components_dir.join(name);
                if tokio::fs::read_to_string(&dest).await.ok().as_ref() == Some(content) {
                    info!("[{}] {} is unchanged", id, dest.display());
                    continue;
                }
                if let Some(b) = builder.as_mut() {
                    b.record(&dest).await.with_context(|| {
                        format!("step '{}': failed to back up {}", id, dest.display())
                    })?;
                }
                write_atomic(&dest, content).await.with_context(|| {
                    format!("step '{}': failed to write {}", id, dest.display())
                })?;
                info!("[{}] installed {}", id, dest.display());
            }

            let mut outcome = StepOutcome {
                reload_recipe: self.reload,
                ..Default::default()
            };
            if let Some(b) = builder {
                outcome.payload = backup::to_payload(&b.finish())?;
            }
            Ok(outcome)
        })
    }

    fn rollback<'a>(
        &'a self,
        ctx: &'a StepCtx,
        payload: &'a toml::Value,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let manifest = backup::from_payload(payload);
            if manifest.is_empty() {
                return Ok(());
            }
            info!(
                "[{}] restoring {} recipe(s) from backup",
                ctx.step_id,
                manifest.entries.len()
            );
            backup::restore(&manifest)
                .await
                .with_context(|| format!("step '{}': failed to restore backup", ctx.step_id))
        })
    }

    fn has_rollback(&self) -> bool {
        self.backup
    }
}

async fn recipe_files(src: &Path) -> Result<Vec<PathBuf>> {
    if !tokio::fs::metadata(src).await?.is_dir() {
        return Ok(vec![src.to_path_buf()]);
    }
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if entry.file_type().await?.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("toml")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// The `.tmp` suffix keeps a half-written file out of `load_components`.
async fn write_atomic(dest: &Path, content: &str) -> Result<()> {
    let tmp = dest.with_extension("toml.tmp");
    tokio::fs::write(&tmp, content).await?;
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::builtin_registry;

    const RECIPE: &str = r#"
schema = 1
[[steps]]
id = "sync"
type = "recipe_sync"
src = "x"
[[steps]]
id = "swap"
type = "shell"
command = ["true"]
"#;

    struct Fixture {
        dir: PathBuf,
        ctx: StepCtx,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("arch-rs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::create_dir_all(dir.join("components")).unwrap();
            let ctx = StepCtx {
                step_id: "sync".into(),
                component: "comp".into(),
                base_dir: dir.clone(),
                components_dir: dir.join("components"),
                is_self_update: false,
            };
            Self { dir, ctx }
        }

        fn src(&self, name: &str, content: &str) {
            std::fs::write(self.dir.join("src").join(name), content).unwrap();
        }

        fn installed(&self, name: &str) -> Option<String> {
            std::fs::read_to_string(self.ctx.components_dir.join(name)).ok()
        }

        fn step(&self, reload: bool) -> Box<dyn Step> {
            let mut body = toml::Table::new();
            body.insert("type".into(), RecipeSync::KIND.into());
            body.insert("id".into(), "sync".into());
            body.insert(
                "src".into(),
                self.dir.join("src").to_string_lossy().into_owned().into(),
            );
            body.insert("reload".into(), reload.into());
            let cfg: crate::config::StepConfig = toml::Value::Table(body).try_into().unwrap();
            builtin_registry()
                .build(&cfg, &crate::interp::Env::new())
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn installs_recipes_and_rolls_them_back() {
        let f = Fixture::new("install");
        std::fs::write(f.ctx.components_dir.join("comp.toml"), "old").unwrap();
        f.src("comp.toml", RECIPE);
        f.src("other.toml", RECIPE);
        f.src("notes.txt", "ignored");

        let step = f.step(true);
        let outcome = step.apply(&f.ctx).await.unwrap();
        assert!(outcome.reload_recipe);
        assert_eq!(f.installed("comp.toml").as_deref(), Some(RECIPE));
        assert_eq!(f.installed("other.toml").as_deref(), Some(RECIPE));
        assert!(f.installed("notes.txt").is_none());

        step.rollback(&f.ctx, &outcome.payload).await.unwrap();
        assert_eq!(f.installed("comp.toml").as_deref(), Some("old"));
        assert!(f.installed("other.toml").is_none());
    }

    #[tokio::test]
    async fn invalid_recipe_writes_nothing() {
        let f = Fixture::new("invalid");
        f.src("comp.toml", RECIPE);
        f.src("other.toml", &RECIPE.replace("\"shell\"", "\"nope\""));

        let err = f.step(false).apply(&f.ctx).await.unwrap_err();
        assert!(format!("{err:#}").contains("'nope'"), "{err:#}");
        assert!(f.installed("comp.toml").is_none());
    }

    #[tokio::test]
    async fn reload_needs_the_running_step_in_the_new_recipe() {
        let f = Fixture::new("noid");
        f.src(
            "comp.toml",
            &RECIPE.replace("id = \"sync\"", "id = \"renamed\""),
        );

        assert!(f.step(true).apply(&f.ctx).await.is_err());
        assert!(f.installed("comp.toml").is_none());
        assert!(f.step(false).apply(&f.ctx).await.is_ok());
    }
}
