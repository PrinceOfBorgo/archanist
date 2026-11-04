# Archanist

[![Github Link][github badge]][github link]
[![Cargo Build & Test][build & test badge]][build & test link]
[![Release][release badge]][release link]
[![Codecov][codecov badge]][codecov link]
[![Buy Me A Coffee][bmc badge]][bmc link]

Archanist is a small, application-agnostic engine that runs a **step
pipeline** to install or upgrade a service, records what it did on
disk so partial failures can be retried or rolled back, and knows how
to update its own container image.

Archanist is not opinionated about *what* it deploys. You write a
**recipe** (a TOML file) that lists the steps to run for a component,
and Archanist executes them, records their outcomes, and lets you
manually roll back a failed run.

## Documentation

| File                                          | Contents                                                                              |
| --------------------------------------------- | ------------------------------------------------------------------------------------- |
| [getting-started.md](docs/getting-started.md) | Build Archanist, initialize a workspace, run your first update.                       |
| [recipes.md](docs/recipes.md)                 | The main config, component recipes, `[vars]`, interpolation, release sources, schema. |
| [steps.md](docs/steps.md)                     | Full reference of every built-in step kind and its options.                           |
| [operations.md](docs/operations.md)           | CLI, state file, rollback, blocklist, self-update mechanics.                          |
| [authoring.md](docs/authoring.md)             | For application authors: how to ship an Archanist recipe with your release.           |

## Quick mental model

- **Component** - a thing Archanist manages (a bot, a web service, a
  database migration set, or Archanist itself). Backed by one recipe
  file under `components/`.
- **Recipe** - TOML file that declares how to discover the latest
  version of a component and the ordered list of **steps** that install
  or upgrade it.
- **Step** - one action in a pipeline. Every step has a `type` (e.g.
  `download`, `db_migrate`, `docker_swap`), an `id` used for state
  tracking, and a type-specific body.
- **State** - `state.toml` records `current_version`,
  `previous_version`, the last release check, the most recent update
  attempt (with per-step payloads for rollback), and the blocklist.
- **Attempt** - one `update` invocation for one component. If any step
  fails, the pipeline aborts, the target version is added to the
  blocklist so the same broken version isn't retried automatically,
  and you can run `archanist rollback <component>` to undo the
  successfully-applied steps.
- **Interpolation** - every string in a recipe supports `${...}`
  substitution. Available variables: `${version}`,
  `${current_version}`, `${component}`, `${vars.X}` for anything
  declared in the component's `[vars]` table, and `${env.NAME}` for
  process environment variables. Literal `$` is escaped as `$$`;
  bare `{...}` (no leading `$`) passes through untouched so
  step-local placeholders like `db_migrate`'s `{file}` / `{name}`
  coexist with pipeline vars.

## Repository layout

```
.
├── Cargo.toml
├── config.toml             # example main config
├── components/             # component recipes
│   ├── archanist.toml      # self-update recipe
│   └── myapp.toml          # example third-party recipe
├── docs/                   # this documentation
└── src/                    # engine source
```

Recipes shipped in this repository under `components/` are the ones
this instance manages. Application authors can ship a recipe file with
their release artifact - see [authoring.md](docs/authoring.md).

## Requirements

- Rust 1.85+ (edition 2024) to build from source.
- Docker only if you use the `docker_swap` step or run Archanist
  itself as a container.

## License

MIT - see [LICENSE](LICENSE).

[github badge]: https://img.shields.io/badge/github-PrinceOfBorgo%2Farchanist-8da0cb?logo=github&logoColor=white
[github link]: https://github.com/PrinceOfBorgo/archanist

[build & test badge]: https://img.shields.io/github/actions/workflow/status/PrinceOfBorgo/archanist/ci.yml?logo=githubactions&logoColor=white&label=Cargo%20Build%20%26%20Test
[build & test link]: https://github.com/PrinceOfBorgo/archanist/actions/workflows/ci.yml

[release badge]: https://img.shields.io/github/actions/workflow/status/PrinceOfBorgo/archanist/release.yml?logo=githubactions&logoColor=white&label=Release
[release link]: https://github.com/PrinceOfBorgo/archanist/actions/workflows/release.yml

[codecov badge]: https://img.shields.io/codecov/c/github/PrinceOfBorgo/archanist?logo=codecov&logoColor=white
[codecov link]: https://codecov.io/gh/PrinceOfBorgo/archanist

[bmc badge]: https://img.shields.io/badge/Buy%20Me%20A%20Coffee-Donate-FFDD00?logo=buymeacoffee&logoColor=white
[bmc link]: https://coff.ee/princeofborgo
