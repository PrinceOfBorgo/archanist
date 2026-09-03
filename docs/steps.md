# Step reference

Every entry in a component's `[[steps]]` array is one **step**. This
document is the reference for the built-in step kinds: their
body fields, defaults, behavior on `apply`, behavior on `rollback`
(where meaningful), and - for the ones that support it - behavior on
`is_satisfied` (used by `archanist check`).

For the shared `id` / `type` fields, `${...}` interpolation, and how
steps fit into a recipe, see [recipes.md](recipes.md).

Steps that don't override the default no-op `rollback` are still
listed by `archanist rollback` but tagged `(no-op)`.

- [`shell`](#shell)
- [`download`](#download)
- [`http_health`](#http_health)
- [`copy_files`](#copy_files)
- [`config_merge`](#config_merge)
- [`parse_text`](#parse_text)
- [`db_migrate`](#db_migrate)
- [`docker_swap`](#docker_swap)

---

## <a id="shell"></a> `shell`

Run an arbitrary command. The escape hatch for anything not covered
by a dedicated step kind.

### Body

| Field     | Type   | Default          | Notes                                                                   |
| --------- | ------ | ---------------- | ----------------------------------------------------------------------- |
| `command` | string | (required)       | Passed to the selected shell as a single command string.                |
| `shell`   | string | platform default | `cmd` (Windows default), `sh` (POSIX default), `pwsh`, or `powershell`. |

### Behavior

- `apply` spawns `<shell> <flags> <command>` and waits. Non-zero exit
  is a failure.
- `rollback` - no-op (labeled `(no-op)` in the rollback log).
- `is_satisfied` - default `false`; a shell step is always considered
  work to do by `archanist check`.

### Example

```toml
[[steps]]
id      = "restart-service"
type    = "shell"
shell   = "pwsh"
command = "Restart-Service -Name myapp"
```

---

## <a id="download"></a> `download`

Fetch a file over HTTP into a target path.

### Body

| Field  | Type   | Default    | Notes                                                                                 |
| ------ | ------ | ---------- | ------------------------------------------------------------------------------------- |
| `url`  | string | (required) | HTTP or HTTPS URL. Any status ≠ 2xx fails the step.                                   |
| `dest` | string | (required) | Local path to write. Missing parent directories are created. Existing files replaced. |

### Behavior

- `apply` downloads the URL body to `dest` in one shot, with a
  30-second timeout.
- `rollback` - no-op.
- `is_satisfied` - default `false`.

### Example

```toml
[[steps]]
id   = "download_bundle"
type = "download"
url  = "https://example.com/releases/v${version}/bundle.zip"
dest = "${vars.staging_dir}/bundle-v${version}.zip"
```

---

## <a id="http_health"></a> `http_health`

Poll an HTTP endpoint until it returns the expected status. Usually
run right after `docker_swap` to gate on the new container being
ready before the pipeline moves on.

### Body

| Field             | Type    | Default    | Notes                                           |
| ----------------- | ------- | ---------- | ----------------------------------------------- |
| `url`             | string  | (required) | Endpoint to probe.                              |
| `expected_status` | integer | `200`      | HTTP status code that means "healthy".          |
| `timeout_secs`    | integer | `60`       | Total wait budget. If exceeded, the step fails. |
| `interval_secs`   | integer | `2`        | Sleep between attempts.                         |

### Behavior

- `apply` loops with `GET url` until the status matches
  `expected_status` or `timeout_secs` elapses. Per-request timeout is
  10s.
- `rollback` - no-op.
- `is_satisfied` - a single 3-second `GET`; returns `true` if the
  response status matches `expected_status`. Any network or status
  mismatch returns `false`.

### Example

```toml
[[steps]]
id              = "wait_healthy"
type            = "http_health"
url             = "http://localhost:8080/health"
expected_status = 200
timeout_secs    = 120
```

---

## <a id="copy_files"></a> `copy_files`

Copy a file, or recursively mirror a directory tree, from `src` to
`dest`. Missing destination parents are created; existing files are
overwritten.

### Body

| Field  | Type   | Default    | Notes                                                                         |
| ------ | ------ | ---------- | ----------------------------------------------------------------------------- |
| `src`  | string | (required) | Source path. Can be a file or a directory.                                    |
| `dest` | string | (required) | Destination path. If `src` is a directory, `dest` becomes its recursive copy. |

### Behavior

- `apply` copies. Sources that don't exist fail the step; sources that
  are neither files nor directories fail explicitly.
- `rollback` - no-op.
- `is_satisfied` - default `false`.

### Example

```toml
[[steps]]
id   = "sync_locales"
type = "copy_files"
src  = "${vars.bundle_dir}/locales"
dest = "${vars.install_dir}/locales"
```

---

## <a id="config_merge"></a> `config_merge`

Deep-merge a TOML `patch` file into a `target` file. Uses `toml_edit`
so the target's comments and key ordering are preserved.

### Body

| Field    | Type   | Default    | Notes                                             |
| -------- | ------ | ---------- | ------------------------------------------------- |
| `target` | string | (required) | Path to the TOML file to modify in place.         |
| `patch`  | string | (required) | Path to the TOML file whose values are merged in. |

### Behavior

- `apply` deep-merges: values in `patch` overwrite values in `target`;
  nested tables recurse; missing keys are added.
- `rollback` - no-op.
- `is_satisfied` - default `false`.

### Example

```toml
[[steps]]
id     = "merge_new_keys"
type   = "config_merge"
target = "${vars.install_dir}/config/${vars.profile}.toml"
patch  = "${vars.bundle_dir}/config-additions.toml"
```

---

## <a id="parse_text"></a> `parse_text`

Read a text file, extract data with regex, publish the results as
interpolation variables for subsequent steps. Driver-agnostic - no
baked-in knowledge of any format.

### Body

| Field     | Type           | Default    | Notes                                                                      |
| --------- | -------------- | ---------- | -------------------------------------------------------------------------- |
| `file`    | string (path)  | (required) | Path to the input file. Relative paths resolve against `base_dir`.         |
| `section` | table          | none       | Region bounds - see below.                                                 |
| `strip`   | array<string\> | `[]`       | Regexes whose matches are removed from the working text before extraction. |
| `extract` | array<table\>  | (required) | One or more extraction rules - at least one is required.                   |

### `section`

Optional bounds that clip the working text before extraction. All
three fields are independent regex patterns, compiled with multiline
mode so `^` / `$` match line boundaries.

| Field         | Notes                                                                                                                          |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `start`       | Drop everything before the FIRST match (the match itself is kept). Errors if the regex doesn't match.                          |
| `start_after` | Like `start`, but the cursor is moved PAST the match. Silently no-ops when the regex doesn't match.                            |
| `end`         | In the remaining text, drop from the FIRST match onward (the match itself is dropped). Searched starting AFTER the first line. |

### `extract`

Each `[[extract]]` rule captures a set of variables. The pattern MUST
declare one or more **named** capture groups (`(?<name>...)`) - each
becomes one exported variable.

| Field      | Type   | Default    | Notes                                                                                    |
| ---------- | ------ | ---------- | ---------------------------------------------------------------------------------------- |
| `pattern`  | string | (required) | Regex applied with `captures_iter`. Must have ≥ 1 named group.                           |
| `prefix`   | string | `""`       | Prepended to each exported name. `prefix = "vars."` + group `foo` → `vars.foo`.          |
| `dedupe`   | bool   | `false`    | Remove duplicates per group before joining.                                              |
| `sort`     | bool   | `false`    | Lexicographically sort per group before joining.                                         |
| `join`     | string | `"\n"`     | Separator placed between collected items when the group is published as a single string. |
| `required` | bool   | `false`    | Fail the step if ANY named group produced zero non-empty captures.                       |

### Behavior

- `apply` reads the file, applies `section` bounds, then `strip`
  patterns, then each `[[extract]]` rule in order. Each named capture
  group in each rule is collected across all matches, optionally
  deduped and sorted, joined by `join`, and published as
  `${<prefix><group_name>}` for later steps.
- `rollback` - no-op.
- `is_satisfied` - default `false`.

### Example - pull migration files out of a markdown table

```toml
[[steps]]
id   = "select_migrations"
type = "parse_text"
file = "${vars.bundle_dir}/MIGRATIONS.md"
section = { start = "## Migration Reference", end = "^## " }
strip = ["~~`[^`]+`~~"]

[[steps.extract]]
pattern = "`(?<migrations>[^`]+\\.sql)`"
prefix  = "vars."
dedupe  = true
sort    = true
```

Later steps consume the result via `${vars.migrations}` - a
newline-separated list of migration filenames.

---

## <a id="db_migrate"></a> `db_migrate`

Driver-agnostic forward-only migration runner with per-run
persistence. Sources migrations either from a glob or from an explicit
list; runs each file through a user-supplied `command` template; skips
files that succeeded on a previous attempt; optionally reverses each
file with a `rollback_command`.

### Body

| Field              | Type                    | Default        | Notes                                                                                       |
| ------------------ | ----------------------- | -------------- | ------------------------------------------------------------------------------------------- |
| `glob`             | string                  | none           | Filesystem glob for migration files. Mutually exclusive with `files`.                       |
| `files`            | array<string> \| string | none           | Explicit list, or a single string split on `split`. Mutually exclusive with `glob`.         |
| `split`            | string                  | `"\n"`         | Used to split `files` when it's a delimited string. Whitespace is trimmed; empties dropped. |
| `base`             | string                  | `ctx.base_dir` | Prefix for non-absolute paths in `files` / `glob`.                                          |
| `command`          | array<string>           | (required)     | argv template. Must be non-empty.                                                           |
| `rollback_command` | array<string>           | none           | argv template for reverting a single migration by stem. See below.                          |
| `cwd`              | string                  | none           | Working directory for `command` / `rollback_command`.                                       |

### Step-local placeholders

`command` and `rollback_command` are argv templates. Inside each
element, TWO placeholders are substituted per invocation (per
migration):

- `{file}` - full path to the migration file
- `{name}` - the file's stem (filename without extension)

These are **step-local** and intentionally distinct from the pipeline
`${...}` syntax. Recipe-wide vars are resolved first (before the step
runs); the `{file}` / `{name}` substitution happens inside the step
per iteration.

### Behavior

- `apply` resolves the file list, applies each unapplied migration in
  stem order, and persists the applied-stems set to
  `<base_dir>/.archanist-migrations/<component>/<step_id>.toml` after
  every successful migration. Subsequent runs skip stems already in
  that file.
- `rollback` iterates the stems applied by the current attempt in
  reverse and runs `rollback_command` for each. When
  `rollback_command` is unset, rollback is a warned no-op and the
  migrations stay applied.
- `is_satisfied` - default `false`.

### Example - files list published by an upstream `parse_text`

```toml
[[steps]]
id    = "migrate"
type  = "db_migrate"
files = "${vars.migrations}"                  # newline-separated, produced upstream
base  = "${vars.bundle_dir}/db"

command          = ["psql", "-d", "myapp", "-f", "{file}"]
rollback_command = ["psql", "-d", "myapp", "-f", "${vars.bundle_dir}/db/down/{name}.sql"]
```

### Example - glob against a bundled directory

```toml
[[steps]]
id      = "migrate"
type    = "db_migrate"
glob    = "*.sql"
base    = "${vars.bundle_dir}/db/up"
command = ["psql", "-d", "myapp", "-f", "{file}"]
```

---

## <a id="docker_swap"></a> `docker_swap`

Pull a new image and (re)create a target container.

### Body

| Field            | Type          | Default            | Notes                                                                  |
| ---------------- | ------------- | ------------------ | ---------------------------------------------------------------------- |
| `image`          | string        | (required)         | Image reference. May include a `:tag` - if so, `tag` field is ignored. |
| `container`      | string        | (required)         | Container name to stop/remove/create.                                  |
| `tag`            | string        | `"latest"`         | Image tag. Appended to `image` unless the image already contains `:`.  |
| `volumes`        | array<string> | `[]`               | Passed through as `-v` arguments - host-side paths.                    |
| `env`            | array<string> | `[]`               | `KEY=VALUE` strings, passed as `-e`.                                   |
| `restart_policy` | string        | `"unless-stopped"` | Container restart policy.                                              |
| `self`           | bool          | `false`            | Marks the archanist's own container - see below.                       |

### Behavior

- `apply` connects to the local Docker daemon, records the container's
  current image (for rollback), pulls the new image, stops and
  removes the container, then recreates it from the new image with
  the specified `env` / `volumes` / `restart_policy`. The recorded
  previous image is persisted as the step's rollback payload.
- `rollback` pulls the previous image (from the payload), stops and
  removes the container, then recreates it from the previous image.
  If no previous image was recorded (fresh install), rollback logs a
  warning and skips.
- `is_satisfied` returns `true` when the container is already running
  the target image (`image:tag`). Returns `false` if the daemon isn't
  reachable - safer to over-report work than to silently skip a swap
  that actually needs to happen.

### Self-update

When `self = true` (or the component name matches the main config's
`self_component`), the step signals `exit_after` in its outcome. The
pipeline stops cleanly after this step; the archanist process exits;
the container orchestrator restarts the new archanist image, which
picks up whatever state the previous one wrote before exit.

### Example - regular application container

```toml
[[steps]]
id        = "swap"
type      = "docker_swap"
image     = "ghcr.io/example/myapp"
tag       = "v${version}"
container = "myapp"
env       = ["RUST_LOG=info"]
volumes   = ["${vars.data_dir_host}:/app/data"]

[[steps]]
id              = "wait_healthy"
type            = "http_health"
url             = "http://localhost:8080/health"
expected_status = 200
```

### Example - self-update

```toml
# components/archanist.toml
[release]
type  = "ghcr_auto"
image = "ghcr.io/example/archanist"

[[steps]]
id        = "swap_self"
type      = "docker_swap"
image     = "ghcr.io/example/archanist"
tag       = "v${version}"
container = "archanist"
self      = true             # implies exit_after
volumes   = [
  "${vars.data_dir_host}:/app/data",
  "/var/run/docker.sock:/var/run/docker.sock",
]
```

And in the main config:

```toml
self_component = "archanist"
```
