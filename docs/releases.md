# Releases and upgrades

**English** | [简体中文](zh-CN/releases.md)

Bone releases are published on [GitHub Releases](https://github.com/frelion/bone/releases).
Each release contains platform archives and a `SHA256SUMS` file.

## Verify an archive

On macOS or Linux:

```bash
shasum -a 256 -c SHA256SUMS
tar -xzf bone-darwin-arm64.tar.gz
./bone/bone --version
```

On Windows, use the matching zip archive and verify its SHA-256 with a trusted
local tool before extracting it.

The executable is self-contained with its matching terminal helper and local
semantic-search native runtime. The model itself is intentionally not included; run
`bone setup` after installation if you want semantic search.

Bone's supported runtime is Bun. GitHub Release archives contain standalone Bun
executables; package and source-checkout execution require Bun 1.3.14 or newer.
Running the CLI with Node.js is not supported.

## Release policy

Tags in the form `vX.Y.Z` trigger a six-platform GitHub Release pipeline. It
builds native semantic runtimes, compiles Bun binaries, runs source validation,
and uploads assets only after checksum verification.

npm publication is not active yet. Do not rely on an npm package name as an
upgrade channel until Bone announces its dedicated package scope.
