# SDK

The Bone SDK creates and controls sessions from Node.js applications.

```typescript
import { createAgentSession } from "@frelion/bone-coding-agent";

const { session } = await createAgentSession({
  cwd: process.cwd(),
});

await session.prompt("Summarize this repository.");
```

`DefaultResourceLoader` loads supported local resources from `~/.bone/agent/skills`, `prompts`, and `themes`; trusted project `.bone/skills`, `prompts`, and `themes`; and configured local paths.

Use the public session, model, built-in tool, skill, prompt-template, theme, event-bus, and settings APIs to embed Bone.

The inline extension runtime is internal to Bone modules and tests. It is not a public plugin SDK and cannot load extension files, package manifests, npm/git packages, or Pi aliases.
