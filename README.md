# Bone

**English** | [简体中文](README.zh-CN.md)

Bone is a local-first coding agent built on a maintained fork of Pi. It adds a
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
npm install --ignore-scripts
npm run build
npm run check
npm test
```

The source remains an npm workspace monorepo. Pi-derived internal package names
are an implementation detail during the GitHub Release phase and are not
published by this repository.

## Supply-chain policy

- Direct dependencies use exact versions; `.npmrc` enforces a two-day npm age gate.
- CI installs dependencies with `--ignore-scripts` and verifies build, checks, and tests.
- Generated shrinkwrap and installer lockfiles are validated before release.
- Release artifacts carry SHA-256 checksums and native semantic runtimes are checked before packaging.

## Upstream

Bone keeps Pi as a Git remote named `upstream` for selected source updates. Bone
product changes and releases live in this repository.

## License

MIT
