# Changelog

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

- Declarative component recipes with a step pipeline runner, `${...}`
  interpolation, and per-component `[vars]`.
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
- Backup-based rollback for `copy_files`, `download`, and `config_merge`, plus
  `backup_command` / `restore_command` for `db_migrate`.
- `init`, `config`, `status`, `check`, `update`, `rollback`, `unblock`, and
  `step-kinds` CLI commands with persisted state and a version blocklist.
- Dockerfile for running archanist as a container.

### Changed

- N/A

### Fixed

- N/A
