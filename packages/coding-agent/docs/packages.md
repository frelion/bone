# Bone Packages and Local Resources

Bone is a built-in coding agent, not a third-party extension marketplace. It does not install, remove, list, or update npm/git packages for agent resources.

## Local resources

Bone loads resources that are placed directly in Bone-owned directories:

- `~/.bone/agent/skills/`
- `~/.bone/agent/prompts/`
- `~/.bone/agent/themes/`
- `<project>/.bone/skills/`
- `<project>/.bone/prompts/`
- `<project>/.bone/themes/`

The `skills`, `prompts`, and `themes` arrays in Bone settings may also contain local files or directories. CLI flags such as `--skill`, `--prompt-template`, and `--theme` are temporary local paths.

Project-local resources are subject to the normal project-trust decision. User resources are loaded from the global Bone directory.

## Removed package commands

`bone install`, `bone remove`, `bone uninstall`, `bone list`, and `bone config` are not supported. `bone update` updates Bone itself only; package and extension update flags are rejected.

Existing package settings and directories such as `packages`, `extensions`, `.bone/npm`, and `.bone/git` are left untouched for inspection or manual cleanup. Bone does not read, migrate, delete, or warn about them.

## Pi extensions are not supported

Bone does not load Pi extension packages, `package.json#pi` manifests, Pi SDK import aliases, npm/git extension sources, or `.bone`/`~/.bone` fallback directories. Existing Pi installations remain on disk and are inert.

Bone-owned code can register inline factories internally. This runtime is an implementation detail and is not a package installation or discovery API.
