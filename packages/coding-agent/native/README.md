# Bone native semantic-search engine

This directory contains Bone's platform Node-API addon and dynamic libraries for the fixed local semantic-search model.

Release CI builds a CPU-only CrispEmbed `v0.15.0` / ggml Node-API addon for each supported
target: `darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`, and
`win32-arm64`. Bone carries a small downstream change that calls CrispEmbed's existing
`core_gguf::load_weights(..., try_mmap = true)` path. This makes the CPU backend hold
GGUF weights in a shared read-only mapping instead of copying them into a second native
allocation.

Source provenance:

- CrispEmbed `v0.15.0` (`77f40f325747267fb633badcfe0118650a00e340`), MIT
- CrispEmbed ggml submodule `0714117daca2471b00e09554c7eaa74a06b0b2c5`, MIT

The addon runs inside Bone's dedicated Node Worker Thread and dynamically loads the
target's CrispEmbed library plus its three adjacent ggml libraries. No daemon, port,
child process, model provider, or user configuration is involved.
