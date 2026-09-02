# Security

Bone is a local coding agent. Its built-in tools can read files, edit files, and run shell commands with the permissions of the Bone process.

Project trust is an input-loading guard: before a project is trusted, Bone does not load `.bone/settings.json`, `.bone/skills`, `.bone/prompts`, or `.bone/themes`. It does not make repository content, model output, or shell commands safe.

Bone does not execute third-party extension code. Pi extension packages, npm/git resource packages, extension directories, package manifests, and extension SDK imports are unsupported and inert. Legacy `packages` and `extensions` settings fields and old installation directories remain untouched.

Forge tools make authenticated requests only to public GitLab/GitHub endpoints or instances explicitly allowlisted in `forge.json`. They reject cross-host redirects and redact configured tokens from remote errors. Repository workflow policy is loaded only after project trust, and sensitive or destructive Forge writes fail closed when no interactive approval provider is available.

For untrusted or unattended work, use a container, VM, or policy-controlled sandbox with minimal filesystem access, credentials, and network access. Review resulting changes before moving them to a trusted environment.

Report security issues through the repository [Security Policy](https://github.com/frelion/bone/blob/main/SECURITY.md).
