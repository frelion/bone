# Containerization

Run Bone in a container, VM, or other isolated environment when working with untrusted repositories or unattended workflows. Mount only the workspace paths and credentials the task needs.

Bone's built-in tools run with the permissions available inside that environment. Local skills, prompts, themes, settings, and sessions are read from the mounted `.bone` and `~/.bone/agent` directories as usual.

Bone does not load extension files from `~/.bone/agent/extensions`, `.bone/extensions`, package manifests, or `-e` CLI paths. Copying an old Pi extension into a container does not enable it.
