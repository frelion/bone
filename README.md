# Bone

**English** | [简体中文](README.zh-CN.md)

Bone is a local-first coding agent. It adds a
session Side panel, concurrent background conversations, visual provider
configuration, task-model routing, and local semantic memory.

```bash
bone
```

Use `/settings` to configure providers and models. Run `bone setup` once to
download Bone's optional local semantic-search model; normal startup never
downloads it automatically.

Read the [documentation](docs/README.md) for installation, sessions, settings,
local memory, and release guidance.

## Releases

Bone currently ships through [GitHub Releases](https://github.com/frelion/bone/releases).
Each tagged release includes native Bun binaries for macOS, Linux, and Windows,
plus SHA-256 checksums. npm publication is deliberately disabled until Bone has
its own package scope and upgrade channel.

## Development

```bash
bun install --ignore-scripts
bun run build
bun run check
bun run test
```

For a source checkout used every day, `bun run dev:install-hook` installs
local-only post-commit and post-merge hooks for the primary checkout. They build
the current-platform Bun binary, switch the local `bone` command atomically after
a successful build, and keep the previous binary if the build fails. Events from
other worktrees do not trigger an install. The package `prepare` step preserves
this hook when dependencies are installed again, including from another worktree.
Set `BONE_SKIP_LOCAL_INSTALL=1` to skip one install. Use
`bun run dev:uninstall-hook` to restore the clone's previous Git hook path and
`bone` command.

The source is a Bun workspace monorepo. Its internal package names are an
implementation detail during the GitHub Release phase and are not published by
this repository. Bun 1.3.14 or newer is the only required JavaScript runtime and
toolchain; release archives contain standalone Bun executables.

## Supply-chain policy

- Direct dependencies use exact versions; `bunfig.toml` enforces a two-day registry age gate.
- CI installs dependencies with `--ignore-scripts` and verifies build, checks, and tests.
- `bun.lock` is the only dependency lockfile and is validated with frozen installs before release.
- Release artifacts carry SHA-256 checksums and native semantic runtimes are checked before packaging.

## License

MIT
