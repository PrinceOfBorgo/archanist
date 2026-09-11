# Operations

This document covers the operator-facing surface: every CLI
subcommand, the `state.toml` layout, the rollback machinery, how the
blocklist works, and the self-update mechanics for running archanist
as its own container.

For the recipe format and the fields each step accepts, see
[recipes.md](recipes.md) and [steps.md](steps.md). For a
walkthrough from an empty checkout to a first successful update, see
[getting-started.md](getting-started.md).

- [Operations](#operations)
  - [ CLI subcommands](#-cli-subcommands)
    - [`update` - pipeline lifecycle](#update---pipeline-lifecycle)
  - [ Logging](#-logging)
  - [ The `state.toml` file](#-the-statetoml-file)
    - [Layout](#layout)
    - [Component fields](#component-fields)
    - [`last_attempt`](#last_attempt)
  - [ Rollback](#-rollback)
    - [Per-step rollback behavior](#per-step-rollback-behavior)
    - [Retry after rollback](#retry-after-rollback)
  - [ Blocklist](#-blocklist)
  - [ Self-update](#-self-update)
    - [Life cycle of a self-update](#life-cycle-of-a-self-update)
    - [What the recipe must do](#what-the-recipe-must-do)
  - [ Running archanist as a container](#-running-archanist-as-a-container)

---

## <a id="cli-subcommands"></a> CLI subcommands

All commands accept `--config <path>` (default `config.toml`) and
`-v` / `-vv` for extra console verbosity.

| Command                                   | Purpose                                                                                                                                                     |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `archanist init`                          | Write a default `config.toml` at the target path if it doesn't already exist.                                                                               |
| `archanist config`                        | Open a read-only tui viewer of the loaded config and state.                                                                                                 |
| `archanist status [<component>]`          | Print recorded state (versions, last check, blocklist). Omit `<component>` to list all.                                                                     |
| `archanist check [<component>]`           | Query the release source for the latest version; when an update is available, probe every step's `is_satisfied` and report `N of M satisfied, K would run`. |
| `archanist update [<component>]`          | Check for updates and apply them if a newer version is available. Aborts on the first step failure; auto-blocks the failed version.                         |
| `archanist rollback <component>`          | Walk the last attempt's completed steps in reverse and invoke each step's `rollback` handler.                                                               |
| `archanist unblock <component> <version>` | Remove `<version>` from the component's blocklist so `update` can retry it.                                                                                 |
| `archanist step-kinds`                    | List all built-in step type names.                                                                                                                          |

### `update` - pipeline lifecycle

For each component (all, or the one named on the command line):

1. Query the release source. Skip if no `[release]` block, or if the
   query fails (message logged, no state written).
2. Compare `latest` against `current_version`. Skip with "up to date"
   if not newer.
3. Skip with a blocklist message if `latest` is in the blocklist.
4. Seed the interpolation env with `${version}`, `${current_version}`,
   `${component}`, and expand `[vars]` two-pass.
5. Start a new `UpdateAttempt` in state with one `StepRun` slot per
   configured step (all `Pending`). Persist.
6. For each step: mark `Running`, persist, invoke `apply`, on success
   mark `Done` with the outcome payload, persist.
7. On success at end: mark attempt `Success`, set `previous_version =
   current_version`, `current_version = latest`.
8. On step failure: mark the running step `Failed` with the error
   message, mark attempt `Failed`, add `latest` to the blocklist,
   propagate the error to the CLI.

Rollback is **not** automatic. Run `archanist rollback` explicitly.

---

## <a id="logging"></a> Logging

Two sinks, both configured in `config.toml`:

- **Console**: level from `log_level` (`info` by default). Written to
  stdout with a timestamped tracing format.
- **File**: written under `log_dir/<log_file_prefix>.<date>.log` when
  `log_dir` is set. Level from `log_file_level`. Rotation from
  `log_rotation` (`daily` / `hourly` / `never`).

Every run wraps its span with a `run_id` of the form
`run-YYYYMMDD-HHMMSS`, so log lines from the same invocation are
easy to correlate:

```
2026-09-04T20:33:02.832374Z  INFO run{run_id=run-20260904-203302}: archanist::pipeline: running pipeline for component 'myapp' (5 step(s))
```

---

## <a id="state-toml"></a> The `state.toml` file

Written by archanist after every step. Never edit it by hand while
archanist is running.

### Layout

```toml
[components.myapp]
current_version       = "1.2.3"
previous_version      = "1.2.2"
latest_check_version  = "1.2.3"
last_check            = "2026-09-04T20:33:02.831631600Z"
blocklist             = ["1.2.4-broken"]

[components.myapp.last_attempt]
target_version   = "1.2.3"
current_version  = "1.2.2"          # value when the attempt started
started_at       = "2026-09-04T20:33:02.831728300Z"
finished_at      = "2026-09-04T20:33:02.848122500Z"
outcome          = "success"        # "in_flight" | "success" | { failed = { message = "..." } }

[[components.myapp.last_attempt.steps]]
id           = "download_bundle"
kind         = "download"
state        = "done"               # "pending" | "running" | "done" | "failed" | "rolled_back"
payload      = { }
started_at   = "2026-09-04T20:33:02.832876900Z"
finished_at  = "2026-09-04T20:33:02.847462400Z"
```

### Component fields

- `current_version` - the version archanist believes is installed.
  Reset to `previous_version` when a rollback completes every step of
  the attempt.
- `previous_version` - the value `current_version` had before the
  most recent successful `update`.
- `latest_check_version` - result of the most recent `[release]`
  query.
- `last_check` - timestamp of the most recent `[release]` query.
- `blocklist` - versions this component won't auto-install. Populated
  by failed `update` runs (see [Blocklist](#blocklist)) and cleared
  by `archanist unblock`.
- `last_attempt` - the most recent update run (see below). Only one
  attempt is retained.

### `last_attempt`

- `target_version` - the version this attempt was trying to install.
- `current_version` - the component's `current_version` at the moment
  the attempt started. Used by rollback to restore state.
- `outcome` - `in_flight`, `success`, or `{ failed = { message } }`.
  A crashed archanist process leaves `in_flight` behind; a subsequent
  `update` overwrites it.
- `steps` - one entry per step in the component's recipe, in pipeline
  order. Each carries its `state`, the opaque `payload` written by
  `apply` (consumed by `rollback`), and start/finish timestamps.

---

## <a id="rollback"></a> Rollback

`archanist rollback <component>` walks `last_attempt.steps` in reverse
definition order and, for every entry with `state = "done"`, invokes
that step's `rollback` handler. Each rolled-back slot transitions
`done → rolled_back`.

When every previously-`done` step of the attempt is rolled back,
archanist resets `current_version = previous_version.take()`, so
`status` / `check` reflect the actual on-disk situation. A second
`rollback` invocation, having nothing left to undo, reports:

```
myapp: no completed steps in last attempt, nothing to roll back
```

### Per-step rollback behavior

| Step type       | Rollback does...                                                                                                                                                                                                                                                                                                                                                                               |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `shell`         | Nothing (labeled `(no-op)`).                                                                                                                                                                                                                                                                                                                                                                   |
| `container_run` | Nothing (labeled `(no-op)`).                                                                                                                                                                                                                                                                                                                                                                   |
| `download`      | With `backup = true` (default), restores the previous `dest` from the snapshot, or deletes it if the download created it. With `backup = false`, nothing (labeled `(no-op)`).                                                                                                                                                                                                                  |
| `http_health`   | Nothing (labeled `(no-op)`).                                                                                                                                                                                                                                                                                                                                                                   |
| `copy_files`    | With `backup = true` (default), restores every overwritten destination file and deletes every file this step created. With `backup = false`, nothing (labeled `(no-op)`).                                                                                                                                                                                                                      |
| `config_merge`  | With `backup = true` (default), restores `target` to its pre-merge contents. With `backup = false`, nothing (labeled `(no-op)`).                                                                                                                                                                                                                                                               |
| `parse_text`    | Nothing (labeled `(no-op)`).                                                                                                                                                                                                                                                                                                                                                                   |
| `db_migrate`    | If a `restore` list is set, runs each restore command in order to restore the pre-migration snapshot taken by `backup_command`, reverting every migration this attempt applied at once. Otherwise, if `rollback_command` is set, runs it once per stem this attempt applied, newest first, trimming each from the on-disk applied-migrations log. If neither is set, logs a warning and skips. |
| `docker_swap`   | Pulls the previously-recorded image, stops and removes the container, then recreates it from the previous image. If no previous image was recorded (fresh install), logs a warning and skips.                                                                                                                                                                                                  |

File-writing steps (`download`, `copy_files`, `config_merge`) snapshot
the exact paths they touch under
`<base_dir>/.archanist-backups/<component>/<step_id>/` before writing,
keyed by step id. Each attempt wipes and recreates its own backup dir,
so the snapshot always reflects the most recent attempt - which means
`rollback` can undo even a *successful* update, up until the next
attempt overwrites the backup. Set `backup = false` on a step to make
it forward-only (its rollback becomes a no-op).

### Retry after rollback

Rollback marks affected steps as `rolled_back`; the next `update`
starts a fresh `UpdateAttempt` and re-runs the whole pipeline. Nothing
is preserved from the rolled-back attempt except the on-disk state
that each step's `rollback` explicitly maintained (e.g.
`db_migrate`'s applied-migrations log stays trimmed).

---

## <a id="blocklist"></a> Blocklist

When an `update` fails, archanist adds the target version to the
component's `blocklist`. Subsequent `update` invocations skip that
version with:

```
myapp: version 1.2.4 is blocked (unblock with `archanist unblock myapp 1.2.4`)
```

Once the underlying issue is fixed (upstream re-release, config
change, etc.) run:

```pwsh
archanist unblock myapp 1.2.4
```

The next `update` will retry.

The blocklist is per component and does not expire.

---

## <a id="self-update"></a> Self-update

Archanist can update its own container using a normal recipe. Two
knobs cooperate:

1. `self_component` in `config.toml` names the recipe file that
   represents this archanist install.
2. `self = true` on the recipe's `docker_swap` step marks that step as
   the self-replacement.

### Life cycle of a self-update

1. `archanist update` processes the self-component's pipeline like
   any other.
2. When the `docker_swap` self-step succeeds, its `StepOutcome.exit_after`
   is `true`. The pipeline persists state, marks the step `Done`, and
   returns.
3. The archanist process exits cleanly.
4. The container orchestrator (docker, docker-compose, Kubernetes,
   whatever) restarts the container using the newly-pulled image.
5. The new archanist boots against the same `state.toml` and `config.toml`
   volumes and sees the completed attempt.

### What the recipe must do

- Bind-mount the docker socket into the archanist container so it can
  drive the daemon:

  ```toml
  volumes = ["/var/run/docker.sock:/var/run/docker.sock"]
  ```

- Bind-mount a data directory that survives container replacement.
  `state.toml` and any per-step persistence (`.archanist-migrations/`)
  must land there.
- Optionally set `restart_policy = "unless-stopped"` on the swap so
  the daemon respawns the new container.

See [authoring.md](authoring.md) for a full worked example.

---

## <a id="running-as-a-container"></a> Running archanist as a container

Beyond the self-update case, running archanist as a container is the
usual way to deploy it. The image needs:

- The archanist binary as its `ENTRYPOINT`.
- Read access to the docker socket bind-mounted at
  `/var/run/docker.sock` (so `docker_swap` steps can drive the host
  daemon).
- A writable data volume that holds `config.toml`, `state.toml`,
  `components/`, and any per-step persistence.

A minimal `docker-compose.yml`:

```yaml
services:
  archanist:
    image: ghcr.io/example/archanist:latest
    container_name: archanist
    restart: unless-stopped
    volumes:
      - ./archanist-data:/app/data
      - /var/run/docker.sock:/var/run/docker.sock
    working_dir: /app/data
    command: ["archanist", "update"]     # or leave the default entrypoint
```

`./archanist-data` is the host directory that shows up as
`${vars.host_data_dir}` in your recipes.
