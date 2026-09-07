# BONE TUI architecture

Status: implementation contract for the first full-screen, multi-session TUI.

## Purpose

`bone-tui` is a presentation layer for `bone-agent`. One terminal can keep several independent conversations alive, switch between them instantly, and accept input while every session continues running models and tools.

The implementation has four parts:

1. `AgentHost` shares one authenticated model connection.
2. Each `AgentHandle` owns one independent Agent session.
3. One UI loop owns all terminal and presentation state.
4. One observer per session forwards tagged Agent updates to that loop.

There is no frontend Agent state machine, session framework, command bus, or component system.

## Technology

```toml
ratatui = { version = "0.30.2", features = ["unstable-rendered-line-info"] }
crossterm = { version = "0.29", features = ["event-stream", "bracketed-paste"] }
ratatui-textarea = "0.9.2"
futures-util.workspace = true
```

[Ratatui] is immediate-mode: BONE retains its state and event loop, while Ratatui provides layout, terminal-buffer diffing, and a test backend. [Crossterm EventStream] supplies asynchronous terminal events. [ratatui-textarea] handles Unicode-aware multiline editing, wrapping, selection, and undo without adding another application runtime.

The workspace declares and explicitly inherits the Rust version required by these dependencies.

## System shape

```mermaid
flowchart LR
    Config[bone-config] --> Host[AgentHost]
    Host --> A[Agent A]
    Host --> B[Agent B]
    A --> OA[observer A]
    B --> OB[observer B]
    OA --> Q[bounded updates]
    OB --> Q
    Keys[terminal events] --> Loop[one UI loop]
    Q --> Loop
    Loop --> State[App]
    State --> View[pure view]
    View --> Terminal
    Loop -->|post / stop| A
    Loop -->|post / stop| B
```

The boundary is strict:

> `bone-agent` decides what a session does. `bone-tui` decides which session is visible, how its records look, and which user command to send.

The TUI never calls a model or tool, starts work itself, checks plan freshness, or reconstructs Kernel policy.

## AgentHost and sessions

The ChatGPT credential store permits one live endpoint for a credential root. Repeated calls to the single-session `bone_agent::start` would compete for that lease, so multi-session frontends connect once:

```rust,ignore
let host = bone_agent::connect(&config, on_login).await?;
let first = host.start(&workspace, task.clone()).await?;
let second = host.start(&workspace, task.clone()).await?;
```

`AgentHost` owns the cloneable `Endpoint` and configuration manager. It does not store sessions or assign session IDs.

Every `AgentHost::start` creates a fresh workspace tool environment, model selection, Kernel, Runtime, record, and job registry. Agent and tool settings come from a new configuration snapshot. The endpoint and credential lease are shared. The credential root is connection-level configuration and changes on the next `connect`.

The free `bone_agent::start` remains the convenience API for a single session.

## Ownership in bone-tui

Runtime resources and presentation data stay separate:

```rust,ignore
struct LiveSession {
    id: SessionId,
    agent: AgentHandle,
    observer: JoinHandle<()>,
}

struct App {
    sessions: Vec<SessionUi>,
    current: usize,
    workspace: String,
    focus: Focus, // Composer or Sessions
    viewport: Viewport,
}

struct SessionUi {
    id: SessionId,
    conversation: Conversation,
    background_unread: bool,
    state: SessionState, // Opening, Live, or Offline(reason)
    pending_post: Option<String>,
}

struct Conversation {
    projection: Projection,
    composer: TextArea<'static>,
    anchor: Option<ScrollAnchor>, // record cursor + wrapped-line offset
    unread: bool,
    cursor: u64,
}
```

Each conversation owns its draft and scroll position. Switching changes only `App.current`; it never moves text between composers, restarts work, or unsubscribes a background session.

`SessionId` is private to `bone-tui`. It routes events within the current process and is not part of the Agent protocol.

## Observation fan-in

Every session begins with `AgentHandle::observe()`. It atomically returns a Snapshot, its sequence, and a receiver containing only later steps.

A small Tokio task follows each receiver and sends tagged updates through one bounded channel, currently sized at 256:

```rust,ignore
enum SessionUpdate {
    Step { id: SessionId, step: Arc<StepEvent> },
    Reset { id: SessionId, snapshot: Snapshot },
    Closed { id: SessionId },
}
```

The observer accepts only the next sequence. A different sequence or `RecvError::Lagged` makes it call `observe()` again and send `Reset`. A closed Agent stream produces `Closed` and ends the observer.

The bounded channel prevents a slow terminal from turning Agent traffic into unbounded memory. Backpressure may make an observer lag; [Tokio broadcast] reports that gap explicitly, and the atomic reset path makes it safe.

Observers only move observations. They never mutate `App`, draw, post, stop, or shut down a session.

## The UI loop

The UI loop is the sole writer of `App` and the terminal:

```rust,ignore
loop {
    terminal.draw(|frame| view::render(frame, &app))?;

    tokio::select! {
        input = terminal_events.next() => match app.on_event(input?) {
            Action::Post { id, text } => accept_or_mark_offline(id, session(id).post(text).await),
            Action::Stop { id, .. } => accept_or_mark_offline(id, session(id).stop().await),
            Action::NewSession => {
                let id = next_id();
                app.begin_session(id);       // immediately selected with its own draft
                pending.push(open(host.clone(), id));
            }
            Action::Quit => break,
            Action::None => {}
        },
        opened = pending.next(), if !pending.is_empty() => {
            attach_or_mark_offline(opened); // never changes the current selection
        },
        update = updates.recv() => app.apply(update?),
    }
}
```

`post`, `stop`, and `observe` wait only for the Runtime actor to accept a command and execute synchronous `Kernel::step`; they never wait for a model or tool. Direct calls preserve message order without a command worker, while every Agent continues long-running work independently. A command failure marks only its session offline; it does not close the workspace.

The first implementation redraws after each delivered terminal or Agent event. It has no frame timer or animation loop. Agent Runtime already coalesces progress.

`Ctrl-N` always inserts and selects a new `Opening` placeholder with its own composer, then starts the Agent asynchronously. The result carries the preassigned `SessionId`, so out-of-order completions bind to the right placeholder without stealing selection. A submitted message is held as one explicit pending post while the session opens; ordinary typing remains a separate draft. Attach sends the pending post, but removes it only after `post` returns its receipt. A start, observation, or post failure restores that text to the composer before marking the placeholder `Offline`, so accepted-looking input is never lost. Authentication happened before raw terminal mode, so new sessions reuse the connected host.

## Projection

`Projection` is a display cache rather than another Agent model. It stores immutable timeline items, active Work/Review/Tool jobs, a display status, and the `ToolCall` needed to join `JobStarted` with `JobFinished`. The session owns only three frontend lifecycle states: `Opening`, `Live`, and `Offline(reason)`.

Reset rebuilds it from `Snapshot.record`. A normal Step applies only `StepEvent.records`. The normal view does not also consume Publish effects because every Notice is already recorded and would otherwise appear twice.

The timeline contains:

- user messages and Agent replies;
- one compact immutable row for each completed tool;
- errors and Paused or Stopped transitions.

Work and input review appear only in the mutable live tail. Tool calls become short semantic descriptions such as `Reading src/main.rs` or `Searched "Projection" · 17 matches`; raw artifacts and JSON do not enter the chat. Successful tool rows obey `show_progress`; failures and unknown external effects always remain visible. An unknown external effect is tracked by Job ID and disappears from the warning state only when that same job receives a conclusive result. Ordinary cancellation is neutral. `Finished` changes the session badge without adding runtime-log noise to the transcript, and still raises attention when it occurs in a background session.

`RecordEntry.cursor` identifies timeline items. A scroll anchor adds a wrapped-line offset within that item, so every part of one long reply remains reachable. Resize keeps the same timeline item and clamps its row after rewrapping; it does not promise to keep the same character at the top. New items follow the live tail unless the user is reading history; in that case the anchor stays and a simple unread marker appears. Tail rendering measures backward from the newest items, so normal typing does not remeasure the complete history.

## Layout and interaction

At 110 columns or wider, a 28-column session rail appears beside a two-column gutter. Each session uses two rows: a stable number and title, followed by its opening, working, waiting, complete, unread, unresolved-effect, or offline state. An accent edge marks the selected row while the rail owns keyboard focus. Persistent risk has priority over ordinary unread activity.

Below 110 columns the conversation uses the full terminal width and a single header carries `BONE  2/4 · current title`. Background `!`, `●`, and `…` markers keep unresolved, unread, and opening sessions visible. `Ctrl-Left` replaces the conversation with the full-screen session list; `Ctrl-Right` returns. This is the same `Sessions` focus and the same `Up`/`Down` selection used by the wide rail.

The wide main region has only the transcript, bordered composer, and one contextual footer; the rail already supplies its session context, so there is no second header. Narrow mode adds the one-line header above them. The workspace appears only in the footer. User turns use an accent edge; Agent replies use one accent marker. Active work is rendered as the transcript's mutable live tail, so `Thinking` and `Reading view.rs 68%` stay attached to the current turn. It is hidden while the user reads older history. The composer remains usable while work continues.

```text
Composer focus
Ctrl-N          new conversation
Ctrl-Left       focus the wide rail or open the narrow session list
Enter           send
Ctrl-J          insert newline
PageUp/Down     move through history
Ctrl-Home/End   oldest item or live tail
Esc             stop the current session

Sessions focus
Up/Down         select a session
Ctrl-Right      show its composer
Enter/Esc       show its composer

Ctrl-C          exit and close all sessions
```

Focus has only two values, `Composer` and `Sessions`; it routes keys and changes the existing selection edge or composer border. The only session-switching path is `Ctrl-Left`, bare `Up`/`Down`, then `Ctrl-Right`. Wide mode presents `Sessions` as the side rail; narrow mode presents it as a full-screen list. There is no second navigation state, generic focus tree, overlay, mouse input, or animation timer. A restrained blue accent marks focus and active work, while yellow and red are reserved for attention and errors.

Until a message is sent, a session title uses the first non-empty draft line. It then comes from the first non-empty line of the first user message and is clipped to the available width. Naming a session never invokes a model.

## Runtime flows

### Background completion

```text
A starts a tool -> user switches to B -> observer A keeps forwarding
-> App updates A and its unread marker -> switching back reveals the result
```

The switch does not pause A and cannot disturb B's composer.

### Observer gap

```text
observer B detects a gap -> B.observe() -> Reset(B, fresh Snapshot)
```

Only B's Projection is rebuilt. Its record cursor makes newly recovered visible items raise attention correctly. Its draft and scroll anchor, every other session, and all live Agents remain intact.

### Stop

`Esc` targets the current `SessionId`. The Kernel records Stop and revokes autonomous work in that session. Other sessions continue.

### Exit

`Ctrl-C`, terminal EOF, or UI failure leaves the inner terminal scope first. `TerminalSession` disables bracketed paste and restores the cursor, alternate screen, and raw mode.

After the shell is restored, `run` requests shutdown on every `AgentHandle` concurrently. Cleanup time is therefore bounded by the slowest session rather than the sum of their grace periods. Observer tasks finish as their streams close, and `run` returns every `ShutdownReport`.

`LiveSession::drop` aborts its observer. This is normally a no-op after graceful shutdown, and ensures that cancelling the public `run()` future cannot leave a detached observer holding an Agent alive.

## Plain execution and event output

`bone <message>` remains a plain single-session path. It observes before posting and tracks the last printed record cursor. On lag or sequence gap it observes again and prints only newer Snapshot records, so Finished, Paused, and Stopped are neither lost nor duplicated.

The JSONL writer remains an independent consumer of the public observation port. It is diagnostic output, not the TUI's state source.

## Code map

```text
crates/bone-tui/src/
├── main.rs       arguments, config, login, mode selection
├── lib.rs        live sessions, observers, UI loop, shutdown
├── app.rs        App, Conversation, Projection, input actions
├── view.rs       pure responsive rendering
├── terminal.rs   raw/alternate/paste lifetime guard
├── config.rs     presentation settings
└── events.rs     JSONL observation export
```

There are no component traits or generic frontend protocols. A type is split out only when the current implementation gives it an independent job.

## Invariants

- One `AgentHandle` is one independent session; one `AgentHost` may create many.
- Only the UI loop mutates `App` or draws.
- Each observer reports events for exactly one `SessionId`.
- Inactive sessions continue executing and being observed.
- A sequence gap replaces only that session's Agent projection.
- Opening owns a composer immediately; one submitted message waits for attach, remains pending until `post` succeeds, and is restored to the draft on failure.
- A local start, observe, post, or stop failure takes down only its session.
- Reset never replaces a composer, scroll anchor, or another session.
- Presentation consumes records and never duplicates Publish effects.
- The TUI never makes Agent validity or completion decisions.
- Terminal restoration precedes slow Agent cleanup.
- All shutdowns run concurrently and all reports remain observable.
- Cancelling the frontend future cannot detach an observer that keeps a session alive.

## Verification covered

- Two sessions from one host have independent records/jobs, share one endpoint, and read fresh session configuration.
- Tagged A/B updates change only the matching projection.
- Switching preserves each draft and background updates raise attention only on their own session.
- A background completion raises attention and updates its marker without adding a `Finished` timeline row or changing the active composer; selecting it clears background unread.
- Opening sessions retain drafts, queue one explicitly submitted message until its receipt, restore it on failure, and attach without stealing selection.
- A long, automatically wrapped Chinese reply can be paged through line by line.
- Lag resets only the affected session and retains local interaction state.
- [Ratatui TestBackend] proves that 120x28 shows the focused 28-column rail without a duplicate main header, while 80x24 and 40x12 use one compact conversation header and present the same session list full-screen when focused.
- Paste normalization, key repeat, multiline input, and combining characters stay intact.
- Quiet mode hides successful tool noise but keeps failures, cancelled writes with unknown outcomes, and later resolutions.
- Pure reasoning that chooses to wait no longer appears as active work, and current progress messages replace earlier ones without entering history.

## First-version boundary

Sessions live only for the process and share the startup workspace and task settings. Individual close, persistence, restored history, workspace browsing, rename/reorder/search, mouse control, Markdown rendering, streaming text, approvals, and rich artifact rendering remain outside this version.

The Agent currently exposes read-only workspace tools. Shell, patch, write, approval, and question protocols belong in `bone-agent`, never as frontend improvisations.

[Ratatui]: https://ratatui.rs/concepts/rendering/
[Crossterm EventStream]: https://docs.rs/crossterm/0.29.0/crossterm/event/struct.EventStream.html
[Ratatui TestBackend]: https://docs.rs/ratatui/latest/ratatui/backend/struct.TestBackend.html
[ratatui-textarea]: https://docs.rs/ratatui-textarea/0.9.2/ratatui_textarea/
[Tokio broadcast]: https://docs.rs/tokio/latest/tokio/sync/broadcast/
