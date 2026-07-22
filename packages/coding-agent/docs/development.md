# Development

See [AGENTS.md](https://github.com/frelion/bone/blob/main/AGENTS.md) for additional guidelines.

## Setup

```bash
git clone https://github.com/frelion/bone
cd bone
npm install --ignore-scripts
npm run build
```

Run from source:

```bash
/path/to/bone/pi-test.sh
```

The script can be run from any directory. Bone keeps the caller's current working directory.
The repository keeps npm and Node.js as development tooling, but the Bone CLI
itself must run with Bun 1.3.14 or newer.

## Forking / Rebranding

Configure via `package.json`:

```json
{
  "boneConfig": {
    "name": "pi",
    "configDir": ".bone"
  }
}
```

Change `name`, `configDir`, and `bin` field for your fork. Affects CLI banner, config paths, and environment variable names.

## Path Resolution

Three execution modes: Bun package install, standalone Bun binary, and Bun from source.

**Always use `src/config.ts`** for package assets:

```typescript
import { getPackageDir, getThemeDir } from "./config.js";
```

Never use `__dirname` directly for package assets.

## Debug Command

`/debug` (hidden) writes to `~/.bone/agent/pi-debug.log`:
- Rendered TUI lines with ANSI codes
- Last messages sent to the LLM

## Testing

```bash
./test.sh                         # Run non-LLM tests (no API keys needed)
npm test                          # Run all tests
npm test -- test/specific.test.ts # Run specific test
```

## Project Structure

```
packages/
  ai/           # LLM provider abstraction
  agent/        # Agent loop and message types  
  tui/          # Terminal UI components
  coding-agent/ # CLI and interactive mode
```
