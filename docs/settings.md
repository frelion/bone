# Settings and providers

**English** | [简体中文](zh-CN/settings.md)

Open `/settings` to use Bone's modal settings center. Changes are staged in the
overlay: `Ctrl+S` validates and saves without closing it, while `Esc` or Cancel
discards the draft.

## Provider-first configuration

Bone has one user-facing configuration unit: a **Provider**. A provider contains
its connection data, one active authentication method, and its models. There is
no separate Account or API-key Profile concept.

In **Providers & Models**, you can:

1. Choose a built-in provider preset or **Custom / OpenAI Compatible**.
2. Set the display name, Base URL, and API protocol.
3. Configure or replace an API key, or start/logout from OAuth when supported.
4. Add models manually or use **Fetch models** for compatible `/models` endpoints.
5. Expand advanced fields only when needed for headers, compatibility, reasoning,
   thinking levels, or token/cost constraints.

Provider definitions are stored in `~/.bone/agent/models.json`.
Secrets are stored only in global `~/.bone/agent/auth.json` with restrictive file
permissions; they are never written to project settings or `models.json`.

## Scope and task models

The overlay can switch between Global and Project scope. Project scope follows
Bone's trust rules and cannot create credentials.

`/model` is a task assignment menu. Today it configures:

- **Conversation**: the current chat model.
- **Title generation**: the model used by parameterless `/name`, or
  **Follow Conversation**.

Future tasks such as planning and design will reuse the same routing model.
