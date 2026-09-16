# BONE local patch

Source: crossterm 0.28.1, unchanged crates.io release except the Unix terminal input paths.

`src/event/source/unix/mio.rs`: report terminal EOF as `UnexpectedEof`, and return fatal read errors instead of repeatedly reading a closed terminal. Keep `WouldBlock` and `Interrupted` handling unchanged. This fixes both initialization queries and the existing terminal input worker; no new reader or parser is introduced.

BONE regression coverage: `crates/bone-tui/tests/pty_terminal.rs`, PTY master closure during keyboard negotiation and after startup.

`src/terminal/sys/unix.rs`: propagate keyboard-query poll errors. Otherwise the initialization query retries terminal EOF forever even after the event source reports it.
