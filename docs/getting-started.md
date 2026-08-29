# Getting started

This walks you from an empty checkout to a working archanist install
and one successful `update` run.

## 1. Build

```pwsh
cargo build --release
```

The binary lands at `target/release/archanist(.exe)`. Copy it onto
your `PATH` or run it in place - every example below assumes
`archanist` resolves to that binary.

## 2. Initialize a workspace

`archanist init` writes a default `config.toml` into the current
folder:

```pwsh
archanist init
```

Result:

```
Created config.toml
```

`config.toml` is your main config: log level, where the state file
lives, where recipes live, etc. All fields have sensible defaults -
the default `components_dir` is `components`, and archanist will
happily read from it once you create it. See
[recipes.md](recipes.md#main-config) for the full list of keys.

## 3. Inspect the empty workspace

```pwsh
archanist status
```

With no `components/` directory yet, archanist prints:

```
(no components configured; looked in components)
```

## 4. Add a component recipe

Create `components/hello.toml`:

```toml
schema      = 1
description = "Toy example that echoes the latest tag from a public repo"

[release]
type = "github"
repo = "rust-lang/rust-analyzer"

[vars]
dest_dir = "./staging"

[[steps]]
id      = "print_version"
type    = "shell"
command = "echo Would deploy version ${version} to ${vars.dest_dir}"
```

Then:

```pwsh
archanist status hello
```

You'll see `hello` listed with no state yet:

```
=== hello ===
  description: Toy example that echoes the latest tag from a public repo
  (no state yet)
```

## 5. Check for updates

`check` asks the release source for the latest version and records
the result in `state.toml`. When an update is available it also
probes each step's `is_satisfied` to report how much work an `update`
would actually do:

```pwsh
archanist check hello
```

Sample output:

```
hello: checking... current=(none), latest=XYZ (update available)
  0 of 1 step(s) already satisfied, 1 would run
```

The `latest` value depends on whatever tag the upstream repo has
published most recently; treat it as a placeholder.

## 6. Apply the update

```pwsh
archanist update hello
```

The single `shell` step runs, echoes the interpolated command,
persists the outcome to `state.toml`, and the component's
`current_version` is bumped:

```
hello: checking... updating (none) -> XYZ
Would deploy version XYZ to ./staging
hello: updated to XYZ
```

(Interleaved log lines from the tracing subscriber are omitted for
brevity.)

Re-running `archanist update hello` now short-circuits:

```
hello: checking... up to date at XYZ
```

And `archanist check hello` skips the per-step probe line once the
component is on the latest version - no work is expected, so no
per-step breakdown is printed:

```
hello: checking... current=XYZ, latest=XYZ
```

## 7. If something goes wrong

If any step fails, the pipeline aborts, the failing version is added
to the component's blocklist so it isn't retried automatically, and
the state file records exactly which steps did apply. Undo them with:

```pwsh
archanist rollback hello
```

`rollback` walks the last attempt's completed steps in reverse and
invokes each step's `rollback` handler. Steps that don't override the
default no-op rollback (like our toy `shell` step) are still listed,
tagged with `(no-op)`:

```
rolling back step 'print_version' (applied at ...) (no-op)
```

When every completed step in the attempt has been rolled back,
archanist also restores the component's `current_version` to whatever
it was before the attempt started - so subsequent `status` / `check`
runs reflect that the update is no longer in place. A second
`rollback` invocation, having nothing left to undo, reports:

```
hello: no completed steps in last attempt, nothing to roll back
```

See [operations.md](operations.md#rollback) for what each built-in
step's rollback actually does.

To retry a blocked version after fixing the underlying issue:

```pwsh
archanist unblock hello XYZ
```

If the version isn't currently blocked, archanist tells you so:

```
hello: version XYZ was not in the blocklist
```

## Next steps

- [recipes.md](recipes.md) - write real recipes: multiple steps, per-component `[vars]`, release sources beyond GitHub.
- [steps.md](steps.md) - every built-in step kind with all its options.
- [operations.md](operations.md) - running archanist as a container, self-update, state file layout, log configuration.
- [authoring.md](authoring.md) - for application authors shipping recipes alongside their releases.
