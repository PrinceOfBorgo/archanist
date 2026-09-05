# Authoring recipes for your app

If you ship a service that other people deploy, you can hand them an
archanist recipe alongside your release artifact. This document is
the checklist for doing that well: which paths belong to the operator
vs. which belong to your bundle, how to use `parse_text` to select
migrations dynamically, and the conventions that let an operator drop
your recipe into their `components/` directory and have `archanist
update` do the right thing.

Prerequisite reading: [recipes.md](recipes.md) for recipe syntax and
[steps.md](steps.md) for step body fields.

- [Authoring recipes for your app](#authoring-recipes-for-your-app)
  - [ The operator contract](#-the-operator-contract)
  - [ Filesystem conventions](#-filesystem-conventions)
  - [ Container-view vs host-view paths](#-container-view-vs-host-view-paths)
  - [ Bundle layout](#-bundle-layout)
  - [ Selecting migrations dynamically](#-selecting-migrations-dynamically)
  - [ Making `docker_swap` self-update friendly](#-making-docker_swap-self-update-friendly)
  - [ Testing your recipe](#-testing-your-recipe)
  - [ Full worked example](#-full-worked-example)

---

## <a id="operator-contract"></a> The operator contract

Your recipe is a contract with whoever runs it. Make the contract
small and explicit:

- **List the required `[vars]` the operator must set** - install
  directories, data volumes, secrets, anything site-specific - and
  default the rest.
- **Assume the operator's `state.toml` already exists**. Archanist
  writes it on first run; don't try to bootstrap.
- **Assume nothing about the operator's docker setup** - some run
  archanist bare-metal, some in a container with `docker.sock`
  mounted. If your recipe needs docker, say so up front.

A common convention is a `# Operator knobs` comment block at the top
of the recipe:

```toml
# ============================================================
# OPERATOR KNOBS
# Set these to match your deployment before running `update`.
# ------------------------------------------------------------
# host_data_dir : host-side path bind-mounted at /app/data inside
#                 the archanist container. All other paths derive
#                 from this.
# admin_email   : where operational alerts go.
# ============================================================
[vars]
host_data_dir = "/opt/archanist-data"      # OPERATOR: edit me
admin_email   = "ops@example.com"          # OPERATOR: edit me

install_dir      = "/app/data/myapp"
install_dir_host = "${vars.host_data_dir}/myapp"
staging_dir      = "/app/data/staging/myapp-v${version}"
```

---

## <a id="filesystem-conventions"></a> Filesystem conventions

Two directory conventions keep recipes robust:

1. **Staging directory** for bundle contents that are only needed
   during the update. Sits under `<host_data_dir>/staging/` so a
   crashed update leaves inspectable artifacts.
2. **Install directory** where your app actually reads its files.

Path pattern by step type:

| Step           | Path field(s)          | Convention                                                                            |
| -------------- | ---------------------- | ------------------------------------------------------------------------------------- |
| `download`     | `dest`                 | Into `staging_dir`.                                                                   |
| `copy_files`   | `src`, `dest`          | `src` in `staging_dir`, `dest` in `install_dir`.                                      |
| `config_merge` | `target`, `patch`      | `target` in `install_dir` (long-lived), `patch` in `staging_dir` (fresh from bundle). |
| `parse_text`   | `file`                 | Into `staging_dir`.                                                                   |
| `db_migrate`   | `base` / `files`       | `base` inside `staging_dir/db/`.                                                      |
| `docker_swap`  | `volumes` (host paths) | Point at `install_dir_host` - see below.                                              |

---

## <a id="container-view-vs-host-view-paths"></a> Container-view vs host-view paths

When archanist runs in a container and drives the host docker daemon
via `docker.sock`, every path has two views:

- **Container view** - what archanist itself reads and writes. Used
  by `copy_files`, `config_merge`, `download`, `parse_text`,
  `db_migrate` (`base` / `files` / `cwd`).
- **Host view** - what the docker daemon resolves when bind-mounting
  paths into peer containers. Used by `docker_swap` `volumes`.

Handing the daemon a container-view path silently creates an empty
directory on the host and mounts *that*, producing crash-looping peer
containers with missing data. The convention is to declare both in
`[vars]`:

```toml
[vars]
host_data_dir     = "/opt/archanist-data"                # OPERATOR: edit me
install_dir       = "/app/data/myapp"                    # container view
install_dir_host  = "${vars.host_data_dir}/myapp"        # host view (mirror)
```

Then `copy_files` uses `${vars.install_dir}` and `docker_swap`
`volumes` use `${vars.install_dir_host}`.

If your recipe runs archanist bare-metal (not in a container), the
two views collapse - but it costs you nothing to declare both, and
the recipe stays deployment-agnostic.

---

## <a id="bundle-layout"></a> Bundle layout

If you ship a release as a zip / tarball, a suggested layout:

```
myapp-v<version>/
├── DEPLOY.md              # human-readable release notes; also machine-parsable
├── config/
│   └── config.toml        # template for config_merge (adds NEW keys only)
├── db/
│   ├── up/
│   │   ├── 001_init.sql
│   │   └── 002_add_email.sql
│   └── down/
│       ├── 001_init.sql
│       └── 002_add_email.sql
└── bin/
    └── myapp              # compiled binary (if you're not shipping as an image)
```

`DEPLOY.md` is optional but useful - see the next section for how
`parse_text` can turn it into a machine-consumable migration list.

---

## <a id="selecting-migrations-dynamically"></a> Selecting migrations dynamically

If your release notes list which migrations are new in each version,
`parse_text` can extract them and `db_migrate` can consume the list.

`DEPLOY.md` fragment your bundle ships:

```markdown
## Migration Reference

| Version | Migrations                                 | Notes   |
| ------- | ------------------------------------------ | ------- |
| v0.2.0  | `001_init.sql`                             | init    |
| v0.2.3  | ~~`002_add_ts.sql`~~, `003_create_XYZ.sql` |         |
| v0.2.4  | `002_add_ts.sql`, `004_overwrite_X.sql`    | fix 002 |
```

Recipe fragment:

```toml
[[steps]]
id   = "select_migrations"
type = "parse_text"
file = "${vars.staging_dir}/DEPLOY.md"
# Only look at rows AFTER the operator's current version:
section = { start = "## Migration Reference", start_after = "^\\| v${current_version} \\|", end = "^## " }
# Skip strikethroughs (obsoleted migrations):
strip = ["~~`[^`]+`~~"]

[[steps.extract]]
pattern = "`(?<migrations>[^`]+\\.sql)`"
prefix  = "vars."
dedupe  = true
sort    = true

[[steps]]
id      = "migrate"
type    = "db_migrate"
files   = "${vars.migrations}"
base    = "${vars.staging_dir}/db/up"
command = ["psql", "-d", "myapp", "-f", "{file}"]
rollback_command = [
  "psql", "-d", "myapp", "-f",
  "${vars.staging_dir}/db/down/{name}.sql",
]
```

The `section.start_after` regex silently no-ops on a fresh install
(when `${current_version}` is empty and the pattern doesn't match),
so the full migration list runs. Once the operator is on some version,
only the rows for versions above them are considered.

---

## <a id="docker-swap-self-update"></a> Making `docker_swap` self-update friendly

If archanist is managing itself, set `self = true` on the swap step
so the pipeline exits cleanly after the new image is placed:

```toml
[[steps]]
id        = "swap_self"
type      = "docker_swap"
image     = "ghcr.io/example/archanist"
tag       = "v${version}"
container = "archanist"
self      = true                              # implies exit_after
volumes   = [
  "${vars.host_data_dir}:/app/data",
  "/var/run/docker.sock:/var/run/docker.sock",
]
```

Combined with `self_component = "archanist"` in the operator's
`config.toml`, this yields a self-updating archanist container. See
[operations.md](operations.md#self-update) for the full life cycle.

For application containers (not archanist itself), leave `self`
unset - the pipeline continues after the swap so subsequent steps
like `http_health` can gate on readiness.

---

## <a id="testing-your-recipe"></a> Testing your recipe

Before shipping:

1. **Static parse**: `archanist status <component>` loads and
   validates the recipe. Fix schema and field errors here.
2. **Dry check**: `archanist check <component>` exercises your
   `[release]` block and probes `is_satisfied` for each step. Confirm
   the release source resolves and the step count looks right.
3. **Full run in a sandbox**: point archanist at a scratch host_data_dir,
   run `archanist update <component>`, verify each step's effect.
4. **Rollback**: run `archanist rollback <component>` and check that
   every reversible step actually reversed itself. `db_migrate`
   should show the down-migrations executing in reverse order.
5. **Re-update**: run `archanist update <component>` again after
   rollback and verify the pipeline completes cleanly the second
   time.

If any step is inherently irreversible (destructive DDL, replacing
config files without keeping the original), document it in your
`DEPLOY.md` so operators know before they run `update`.

---

## <a id="full-worked-example"></a> Full worked example

Ship this alongside your release, in `components/myapp.toml`:

```toml
schema      = 1
description = "MyApp"

[release]
type = "github"
repo = "example/myapp"

# ============================================================
# OPERATOR KNOBS
# ============================================================
[vars]
host_data_dir = "/opt/archanist-data"          # OPERATOR: edit me

install_dir       = "/app/data/myapp"
install_dir_host  = "${vars.host_data_dir}/myapp"
staging_dir       = "/app/data/staging/myapp-v${version}"

# --- Bundle staging ---
[[steps]]
id   = "download_bundle"
type = "download"
url  = "https://github.com/example/myapp/releases/download/v${version}/bundle-v${version}.zip"
dest = "${vars.staging_dir}.zip"

[[steps]]
id      = "extract_bundle"
type    = "shell"
command = "unzip -o ${vars.staging_dir}.zip -d ${vars.staging_dir}"

# --- Deploy ---
[[steps]]
id   = "sync_locales"
type = "copy_files"
src  = "${vars.staging_dir}/locales"
dest = "${vars.install_dir}/locales"

[[steps]]
id     = "merge_config"
type   = "config_merge"
target = "${vars.install_dir}/config/config.toml"
patch  = "${vars.staging_dir}/config/config.toml"

# --- Database migrations ---
[[steps]]
id   = "select_migrations"
type = "parse_text"
file = "${vars.staging_dir}/DEPLOY.md"
section = { start = "## Migration Reference", start_after = "^\\| v${current_version} \\|", end = "^## " }
strip   = ["~~`[^`]+`~~"]

[[steps.extract]]
pattern = "`(?<migrations>[^`]+\\.sql)`"
prefix  = "vars."
dedupe  = true
sort    = true

[[steps]]
id      = "migrate"
type    = "db_migrate"
files   = "${vars.migrations}"
base    = "${vars.staging_dir}/db/up"
command = ["psql", "-d", "myapp", "-f", "{file}"]
rollback_command = [
  "psql", "-d", "myapp", "-f",
  "${vars.staging_dir}/db/down/{name}.sql",
]

# --- Container swap ---
[[steps]]
id        = "swap"
type      = "docker_swap"
image     = "ghcr.io/example/myapp"
tag       = "v${version}"
container = "myapp"
volumes   = ["${vars.install_dir_host}:/app/data"]

# --- Readiness gate ---
[[steps]]
id              = "wait_healthy"
type            = "http_health"
url             = "http://localhost:8080/health"
expected_status = 200
timeout_secs    = 120
```

That single file gives an operator every ingredient: they set
`host_data_dir`, drop the file into their `components/`, and
`archanist update myapp` handles the rest.
