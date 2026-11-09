# Changelog

## [0.1.5-SNAPSHOT] - Unreleased
### Added
- `recipe_sync` step: installs component recipes into `components_dir` after checking that each one parses and only uses step types the running archanist supports, so a bad recipe never lands on disk. Writes are atomic and backed up for `rollback`. `reload = true` continues the running update with the component's new recipe.

### Changed
- Refactored step kind common interface, adding `StepKind` trait.
- Updated dependencies.

### Fixed
- `update`: when a step rewrote the component's own recipe (e.g. a recipe sync), the remaining steps and `[vars]` still came from the recipe loaded at startup, so recipe changes only took effect one release later. The rest of the run can now use the new recipe: when the step is a `recipe_sync` with `reload = true`, or when the new recipe declares that step as one (so an older recipe that copies its successor in still hands over). Other mid-run recipe changes are logged as a warning and apply from the next run. After a reload, steps already completed in the run are not repeated.
- `update`: after a successful self-update, the old process went on updating the remaining components. It now stops there, so run `update` again to have the new binary handle them.

## [0.1.4] - 2026-11-04
### 🔧 Patch Release
### Added
- `archanist sandbox run|rollback|reset <component>`: rehearse a component's update in a throwaway environment declared by an optional `[sandbox]` section in its recipe - peer service containers (with healthchecks) on a dedicated network, one-time baseline `setup` steps, `[sandbox.vars]` / `[sandbox.env]` overrides, per-step `[sandbox.override.<id>]` patches, and `skip_steps`. State and artifacts live under `<base_dir>/sandbox/<component>/`, never in the real state. A non-skipped `docker_swap` step must override `container`, so a sandbox can't replace the real container.

### Changed
- N/A

### Fixed
- N/A

## [0.1.3] - 2026-09-20
### 🔧 Patch Release
### Added
- `docker_swap`: optional `network` and `extra_hosts` fields, so a recreated service container can join a user network and resolve host aliases (e.g. `host.docker.internal:host-gateway`). Both are persisted in the rollback payload.

### Changed
- N/A

### Fixed
- `docker_swap`-managed containers could not reach services bound to the host (e.g. SurrealDB at `host.docker.internal`) on Linux, because the step had no way to add a `host-gateway` mapping. On startup the target would panic (exit 101) and Docker would restart-loop it. The `travel-rs` component now sets `extra_hosts = ["host.docker.internal:host-gateway"]` on its swap step.

## [0.1.2] - 2026-09-20
### 🔧 Patch Release
### Added
- `parse_text`: optional `version_filter` that keeps only lines whose captured semver is strictly newer than a reference version (`newer_than`). A non-semver reference (e.g. the `(none)` fresh-install sentinel) disables filtering so a first install still sees every row. Lets migration-selection recipes resolve correctly even when updating from a release absent from the reference table.

### Changed
- N/A

### Fixed
- N/A

## [0.1.1] - 2026-09-19
### 🔧 Patch Release
### Added
- Multi-platform support.

### Changed
- Updated dependencies.

### Fixed
- N/A

## [0.1.0] - 2026-09-14
### 🔧 Patch Release
### Added
- Declarative component recipes with a step pipeline runner, `${...}` interpolation, and per-component `[vars]`.
- Built-in steps:
  - `shell` - run an arbitrary command (escape hatch).
  - `download` - fetch a file over HTTP into a target path.
  - `http_health` - poll an endpoint until it returns the expected status.
  - `copy_files` - copy a file or mirror a directory tree.
  - `config_merge` - deep-merge a TOML patch into a target, preserving comments.
  - `container_run` - run a one-shot helper container to completion.
  - `parse_text` - extract data from text via regex into pipeline vars.
  - `db_migrate` - forward-only, driver-agnostic migration runner.
  - `docker_swap` - pull a new image and (re)create the target container.
- Release sources: GitHub, GHCR, Docker Hub, and pinned versions.
- Backup-based rollback for `copy_files`, `download`, and `config_merge`, plus `backup_command` / `restore_command` for `db_migrate`.
- `init`, `config`, `status`, `check`, `update`, `rollback`, `unblock`, and `step-kinds` CLI commands with persisted state and a version blocklist.
- Dockerfile for running archanist as a container.

### Changed
- N/A

### Fixed
- N/A
