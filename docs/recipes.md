# Recipes

A **recipe** describes one thing archanist manages. This document
covers:

- The main config file (`config.toml`).
- Component recipe files (one per component under `components/`).
- The `[vars]` block and `${...}` interpolation syntax.
- Release sources.
- The optional `[sandbox]` section.
- The `schema` versioning contract.

For the field-by-field reference of every step kind, see
[steps.md](steps.md).

---

## Main config: `config.toml`

Global settings loaded once at startup. `schema` is required;
everything else has a sensible default.

```toml
# REQUIRED. Format version of this file. See "Schema versioning" below.
schema = 1

# Console log level: trace | debug | info | warn | error.
log_level = "info"

# Optional file logging. Set log_dir to enable; omit to disable.
# Files are named <log_file_prefix>.<date>.log.
log_dir         = "logs"
log_file_prefix = "archanist"
log_file_level  = "debug"
log_rotation    = "daily"       # daily | hourly | never

# Path to the state file. Relative paths resolve against the directory
# containing config.toml.
state_file = "state.toml"

# Directory containing per-component .toml files. Relative to config.
components_dir = "components"

# Optional. Name of the component (matching a file under components_dir,
# without the .toml extension) that represents THIS archanist install.
# When set, a `docker_swap` step in that component's recipe with `self =
# true` implies `exit_after` so the process cleanly exits and the new
# container image can take over. Unset by default - archanist won't try
# to self-update.
# self_component = "archanist"
```

### Notes

- `state_file`: archanist writes here after every step. Don't put it
  under a directory managed by another process.
- `components_dir`: doesn't have to exist ahead of time - archanist
  reports "no components configured" if it's missing or empty.

---

## Component recipe

One TOML file per component under `components_dir`. The **file stem**
becomes the component name (e.g. `myapp.toml` → component `myapp`).
CLI references and `${component}` interpolation use this name.

Skeleton:

```toml
# REQUIRED. Format version. See "Schema versioning".
schema = 1

# Optional human-readable label shown in `status` output.
description = "My App"

# How to discover the latest version. Optional: a component with no
# [release] block is skipped by `update` (there's no "latest" to
# compare against).
[release]
type = "github"
repo = "owner/myapp"

# Free-form key/value strings, all typed as string. Every value is
# interpolated (see "Interpolation" below) and then exposed under
# `${vars.KEY}` in every step body.
[vars]
install_dir = "/app/data/myapp"

# Ordered pipeline. Executed top-to-bottom on `update`, walked in
# reverse on `rollback`. See steps.md for the fields each `type`
# accepts.
[[steps]]
id      = "download_bundle"
type    = "download"
url     = "https://example.com/releases/v${version}/bundle.zip"
dest    = "${vars.install_dir}/bundle-v${version}.zip"

[[steps]]
id        = "swap"
type      = "docker_swap"
image     = "ghcr.io/example/myapp"
tag       = "v${version}"
container = "myapp"
volumes   = ["${vars.install_dir}/config:/app/config"]
```

### Step-level fields (common to every kind)

Every `[[steps]]` entry has these two fields regardless of `type`:

| Field  | Type   | Notes                                                                                           |
| ------ | ------ | ----------------------------------------------------------------------------------------------- |
| `id`   | string | Required. Unique within the component. Used in state, logs, and rollback.                       |
| `type` | string | Required. One of the registered kinds - see [steps.md](steps.md) or run `archanist step-kinds`. |

The rest of the fields on a `[[steps]]` entry are step-kind-specific.

### Rollback and failure handling

If any step's `apply` returns an error, the pipeline aborts and the
target version is added to the component's blocklist so archanist
doesn't retry it on the next `update`. Nothing is rolled back
automatically - the operator invokes `archanist rollback <component>`
to undo the steps that did complete, in reverse definition order. See
[operations.md](operations.md#rollback) for what each built-in step's
`rollback` handler actually does.

---

## `[vars]`

Free-form string key/value map, per component. Every value is itself
run through the interpolation engine before use, so a var can
reference the update context (`${version}` etc.) or another var.

Values are stored under `${vars.KEY}` in the interpolation scope.

### Two-pass expansion

`[vars]` values may reference each other:

```toml
[vars]
staging_root = "/app/data/staging"
bundle_dir   = "${vars.staging_root}/myapp-v${version}"
```

Because TOML tables are unordered, archanist runs two passes:

1. Each `vars.*` value is expanded against the currently-known
   variables. Failures fall back to the raw literal.
2. Every `vars.*` value is re-expanded once against the now-complete
   map, resolving cross-references like the one above.

Deeper chains (`A → B → C`) are not resolved by the two-pass. Keep
your var graph shallow - one hop.

---

## Interpolation

Every string in a recipe body is passed through the interpolation
engine before the step sees it. Syntax:

- `${NAME}` - substitution. `NAME` is looked up in the current scope.
  An unresolved reference is a hard error.
- `${env.NAME}` - reads `NAME` from the process environment. The
  pipeline scope wins if it also has a matching `env.NAME` entry, so
  callers can shadow OS values; otherwise the value is fetched from
  `std::env`, and a missing OS var is a hard error.
- `$$` - escaped literal `$`.
- `{...}` (with no leading `$`) - passes through untouched. Reserved
  for step-local placeholders like `db_migrate`'s `{file}` and
  `{name}` (see [steps.md](steps.md#db_migrate)) so those don't
  conflict with pipeline vars.

### Built-in variables

Set by the `update` command before any step runs:

| Name                 | Value                                                                         |
| -------------------- | ----------------------------------------------------------------------------- |
| `${version}`         | Version being installed (the "target"). Comes from the `[release]` source.    |
| `${current_version}` | Currently-installed version, or `(none)` on a fresh install.                  |
| `${component}`       | The component's name (file stem).                                             |
| `${vars.KEY}`        | Value from the recipe's `[vars]` block, post two-pass expansion. See above.   |
| `${env.NAME}`        | Value of the `NAME` process environment variable. Missing OS var is an error. |

### Step-published variables

A step can publish new variables into the scope for later steps in the
same pipeline. The primary producer is `parse_text` - each named
capture group in its `pattern` becomes one exported variable, prefixed
by the rule's `prefix` field:

```toml
[[steps]]
id   = "select_migrations"
type = "parse_text"
file = "${vars.bundle_dir}/MIGRATIONS.md"

[[steps.extract]]
pattern = "`(?<migrations>[^`]+\\.sql)`"
prefix  = "vars."
dedupe  = true
sort    = true

[[steps]]
id       = "migrate"
type     = "db_migrate"
files    = "${vars.migrations}"          # published by the step above
split    = "\n"
command  = ["psql", "-d", "myapp", "-f", "{file}"]
```

To publish something reachable via `${foo}` (no prefix), leave
`prefix = ""` on the extract rule.

See [steps.md](steps.md#parse_text) for the full `parse_text` reference.

---

## Release sources

The `[release]` block tells archanist how to discover the latest
version of a component. Four kinds, discriminated by `type`.

### `github`

```toml
[release]
type = "github"
repo = "owner/repository"
```

Queries the `releases/latest` endpoint on `api.github.com`. Draft and
prerelease releases are ignored (archanist reports "no stable release
available"). A leading `v` on the tag name is stripped, so `v1.2.3`
becomes `1.2.3` for `${version}`.

### `ghcr_auto`

```toml
[release]
type  = "ghcr_auto"
image = "ghcr.io/owner/repository"
```

Convenience wrapper: extracts `owner/repository` from the image path
and delegates to the GitHub Releases API. Fails if the image is not
hosted on `ghcr.io`.

### `docker_hub`

```toml
[release]
type  = "docker_hub"
image = "namespace/name"        # or "library/name" for official images
```

Fetches tags from Docker Hub and returns the highest semver-parseable
one. Non-semver tags (`latest`, git SHAs, `stable`) are ignored.

### `pinned`

```toml
[release]
type    = "pinned"
version = "1.2.3"
```

Always reports the configured value as "latest". Useful for test
fixtures, and to temporarily hold a component at a known-good version
without needing a network-facing release source.

### Version comparison

Semver comparison is used when both `current_version` and the returned
latest are valid semver. Otherwise, plain string inequality decides
"newer" - meaning any string change is considered an update. Mix
semver and non-semver at your own risk.

---

## <a id="sandbox"></a> `[sandbox]`

Optional. Describes a throwaway environment where
`archanist sandbox run <component>` rehearses the real update against
the real latest release without touching the live deployment:

1. Create `network` (if set) and start every `[[sandbox.services]]`
   container on it, waiting for its healthcheck. Existing containers
   are started, not recreated.
2. On the first run only (no sandbox state yet), run the
   `[[sandbox.setup]]` steps to install the baseline that the update
   starts from, then record `current_version = baseline_version`.
3. Run the component's `[[steps]]` through the normal `update` flow,
   minus `skip_steps`, with each `[sandbox.override.<id>]` table merged
   over the matching step body and `[sandbox.vars]` merged over
   `[vars]`. The sandbox blocklist is cleared first, so a failed
   rehearsal can be retried. `recipe_sync` steps install into
   `<base_dir>/sandbox/<component>/components/` and never reload, so
   they can stay in the rehearsal without touching the live recipes.
4. Print a summary (container images, resulting version).

The sandbox keeps its own `state.toml`, migration logs and backups
under `<base_dir>/sandbox/<component>/`. `archanist sandbox rollback`
undoes the last sandbox attempt; `archanist sandbox reset` removes the
service containers, any containers listed in `cleanup_containers`, the
sandbox network, and the `<base_dir>/sandbox/<component>/` directory.

```toml
[sandbox]
baseline_version   = "1.4.0"            # unset = rehearse a fresh install
network            = "myapp-sandbox"
skip_steps         = ["notify_ops"]
cleanup_containers = []                 # extra containers removed on reset

# Shadows ${env.NAME} lookups (credentials, endpoints, ...).
[sandbox.env]
MYAPP_DB_URL = "postgres://app:app@myapp-sandbox-db:5432/app"

# Overrides [vars] entries; redirect every path into the sandbox dir.
[sandbox.vars]
install_dir      = "${sandbox.dir}/myapp"
install_dir_host = "${vars.host_data_dir}/sandbox/${component}/myapp"

# Keys merged over the step with that id. Required for every non-skipped
# docker_swap step (at least `container`), so the real container is safe.
[sandbox.override.swap]
container      = "myapp-sandbox"
network        = "${sandbox.network}"
restart_policy = "no"

[[sandbox.services]]
name        = "myapp-sandbox-db"
image       = "postgres:16"
env         = ["POSTGRES_USER=app", "POSTGRES_PASSWORD=app"]
healthcheck = { test = ["CMD", "pg_isready"], interval_secs = 2, retries = 15 }

[[sandbox.services]]
name  = "myapp-sandbox"                # the "old" deployment the swap replaces
image = "ghcr.io/example/myapp:v${sandbox.baseline_version}"

# Any step kind. Here ${version} = baseline_version (not the release target).
[[sandbox.setup]]
id   = "baseline_schema"
type = "db_migrate"
glob = "*.sql"
base = "${sandbox.dir}/baseline/migrations"
command = ["psql", "${env.MYAPP_DB_URL}", "-f", "{file}"]
```

Extra interpolation variables, available in `[sandbox.vars]`,
overrides, services and setup steps:

| Name                          | Value                                                         |
| ----------------------------- | ------------------------------------------------------------- |
| `${sandbox.dir}`              | `<base_dir>/sandbox/<component>` (archanist's own path view). |
| `${sandbox.network}`          | `network`, or empty.                                          |
| `${sandbox.baseline_version}` | `baseline_version`, or empty.                                 |

### Services

Each `[[sandbox.services]]` entry is a long-lived container the
rehearsal needs alongside the component (a database, the "old"
deployment the swap replaces, ...). Fields:

| Field         | Notes                                                                                                                        |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `name`        | Required. Container name. Also used by `reset` to remove it.                                                                 |
| `image`       | Required. Pulled only when the container is created.                                                                         |
| `command`     | Optional argv. Empty keeps the image's default command.                                                                      |
| `env`         | Optional `KEY=VALUE` entries.                                                                                                |
| `volumes`     | Optional mounts in `source:target[:mode]` form (host paths or named volumes).                                                |
| `healthcheck` | Optional Docker healthcheck: `test` (argv), `interval_secs` (default 2), `timeout_secs` (default 2), `retries` (default 15). |

Services join `network` and always run with restart policy `no`, so a
crashing service doesn't loop. A service that already exists is started
as is, not recreated, so a database keeps its data between runs. When
`healthcheck` is set, `sandbox run` waits until Docker reports the
container healthy before continuing. It fails if the container turns
unhealthy or isn't healthy within about `interval_secs × (retries + 1)`
seconds plus 30 seconds of grace.

### Setup steps

`[[sandbox.setup]]` entries are ordinary steps: any `type` from
[steps.md](steps.md) works, with the same fields. They differ from
`[[steps]]` in three ways:

- **They see the baseline as `${version}`.** During setup, `${version}`
  is `baseline_version` (not the release target) and
  `${current_version}` is `(none)`. That lets setup reuse the same URL
  and path patterns as the real pipeline to fetch the baseline. The
  same string, e.g. `bundle-v${version}.zip`, therefore resolves
  differently during setup and during the update. Use
  `${sandbox.baseline_version}` when you want to name the baseline
  explicitly.
- **They run once.** Setup only runs when the sandbox has no state for
  the component. If it fails, nothing is recorded and the next
  `sandbox run` retries it. After it succeeds, editing setup steps has
  no effect until `archanist sandbox reset <component>`.
- **They aren't rolled back.** Setup isn't part of an update attempt, so
  `sandbox rollback` never undoes it. Use `reset` to start over.

Setup step ids must not match any `[[steps]]` id. Both lists keep
per-step files (such as `db_migrate`'s applied-migrations log) in the
same sandbox directory, keyed by id.

### Validation

The `[sandbox]` section is checked when the recipe loads, so mistakes
fail before anything runs:

- Unknown keys in `[sandbox]`, a service, or a `healthcheck` are
  rejected, so a misspelled `override` can't silently leave a step
  pointed at production. The tables inside `[sandbox.override.<id>]`
  and the bodies of setup steps are checked only by the step itself,
  like any `[[steps]]` body.
- Every id in `skip_steps` and every `[sandbox.override.<id>]` must
  name an existing step, and an override can't change `id` or `type`.
- Every `docker_swap` step that isn't skipped must override
  `container`, so the rehearsal can never replace the live container.
- `network`, if set, must not be empty.

When archanist runs as a container, `${sandbox.dir}` is a container
path; hand helper containers its host equivalent (see
[authoring.md](authoring.md#container-view-vs-host-view-paths)).

---

## Schema versioning

Every archanist config file and every component recipe declares a
`schema` integer at the top. The loader rejects unknown values with a
clear error naming the file and the versions this binary supports.

Currently supported: `schema = 1` for both main configs and component
recipes.

### When the schema bumps

**Breaking (requires a bump):**

- Rename or remove a step-config field.
- Rename a step `type`.
- Change the semantics of an existing field.
- Rename or remove a top-level main-config or recipe field.

**Additive (no bump):**

- Add a new optional field with a default.
- Add a new step `type`.
- Add a new `[release]` `type`.

When archanist introduces a schema bump, it publishes migration notes
in the release changelog and continues to read the previous schema
until the following major release.
