# Bone

Bone is a local-first coding agent with concurrent conversations, a Side
session panel, visual provider configuration, task-model routing, and local
semantic memory.

## Install and run

Download a platform archive from the
[Bone releases page](https://github.com/frelion/bone/releases), place the
`bone` executable on your `PATH`, then run:

```bash
bone
```

Use `/settings` to add a provider and model. Run `bone setup` only when you
want to enable local semantic search; normal startup never downloads a model.

## Configuration

Bone stores global configuration under `~/.bone/agent/`. Provider definitions
are in `models.json`; credentials are stored separately in `auth.json` and are
never written to project settings.

## Network behavior

Bone has no default update, telemetry, model-catalog, share-preview, or Radius
service endpoint. Those optional features activate only when their corresponding
`BONE_*_URL` environment variable is configured:

- `BONE_UPDATE_URL`
- `BONE_TELEMETRY_URL`
- `BONE_MODEL_CATALOG_URL`
- `BONE_SHARE_VIEWER_URL`
- `BONE_RADIUS_URL` or `BONE_RADIUS_ORCHESTRATOR_URL`

Set `BONE_OFFLINE=1` or pass `--offline` to disable startup network operations.

## Documentation

Read the repository documentation for installation, sessions, settings, local
memory, and release guidance:

- [English documentation](../../docs/README.md)
- [中文文档](../../docs/zh-CN/README.md)

## Development

```bash
npm install --ignore-scripts
npm run check
```

Bone is released through GitHub Releases. npm publication is intentionally
disabled until the internal package scope is migrated. The supported CLI
runtime is Bun 1.3.14 or newer; Node.js is used only by the repository's
existing development tooling.
