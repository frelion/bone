# RPC Mode

Start Bone in RPC mode with:

```bash
bone --mode rpc --no-session
```

RPC mode exchanges JSON requests and events over stdin and stdout. It is intended for applications that need to start conversations, send prompts, receive streamed messages, and manage sessions programmatically.

Use the built-in RPC command and event definitions in `src/modes/rpc/rpc-types.ts` as the protocol reference. Local skills and prompt templates remain available under the normal resource-loading rules.

Bone does not expose extension commands, extension UI requests, or a third-party extension protocol through RPC.
