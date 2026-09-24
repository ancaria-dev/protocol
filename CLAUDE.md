# protocol

Workspace rules, target build, release chain, and elevation: see `../CLAUDE.md`.

## Process boundary

- Never move JVM or mod code into the game process. The game is 32-bit; the JVM runs as a separate `java.exe`.
- Native game calls and memory access stay in the injected agent. The agent never talks to the JVM directly.
- Read `docs/PROTOCOL.md` before changing `src/codec.rs` or `src/router.rs`. Where it describes planned behavior, the implementation wins.
- Never add a game address here. `gen/addr.js` comes from coderpack's `tools/addr.py`.

## Transports

- Agent to host: Frida `send()`. Host to agent: `Script::post()` with JSON.
- Host and JVM: newline-terminated UTF-8 frames over two named pipes the host creates before starting the JVM, base `\\.\pipe\ancaria-<random>` passed as `--pipe`. `<base>.in` carries frames to the JVM, `<base>.out` back.
- Keep one pipe per direction. A synchronous handle serialises operations on its file object, so a parked `ReadFile` blocks every write; a duplicated handle shares the file object. The symptom is one frame, then silence.
- Keep the stdin/stdout fallback used without `--pipe`. The test harnesses and manual driving depend on it.
- JVM stdout and stderr are inherited and are the path for Java diagnostics. `jvm.rs` reports an unreadable line as `[coderpack] Could not parse Coderpack output: <line>`.

## Wire invariants

- `END` has no name token. `END 3 cancel=1` once parsed `cancel=1` as a name and dropped every verdict silently. Keep `codec::tests::verdicts_keep_their_fields`.
- The `LOG <level> <text>` form in `docs/PROTOCOL.md` is invalid on the wire: both decoders require a decimal sequence as the second token of every non-`BYE` line. Specify and test one form on both sides before using wire `LOG`.
- `EVT` uses sequence `0`; `ASK` uses a session-local increasing counter; coderpack counts `CMD` separately.
- Send `ASK` only while the agent can still cancel or rewrite the game operation.
- Percent-encode only `%`, space, `=`, LF, CR. Keep fields flat. `flatten` turns a nested value into one JSON field with a warning; that is a protocol bug.
- The router also accepts `BYE` from coderpack and posts `{"type":"mode","ask":false}`. Keep this unless both sides and the spec change together.

## Concurrency and verdicts

- Nothing in `router.rs` may wait for JVM work or a command result. An `ASK` blocks the game thread in Frida's `recv().wait()`.
- Frida's callback converts agent messages and sends frames to the JVM writer channel.
- The JVM reader thread decodes frames and queues work for `pump`.
- Only `pump` owns the `Script` and calls `Script::post()`.
- `VERDICT_DEADLINE` is 250 ms. The watchdog scans every 125 ms and marks an ask overdue only when its age exceeds 250 ms, so the fallback is queued roughly 250 to 375 ms after the ask, plus scheduler and poster-loop delay. Never describe 250 ms as a strict worst case.
- An overdue ask gets an allow verdict with no rewrites and the log line `[host] Coderpack did not answer request <seq> in time. Continuing`. The slow mod loses its veto.
- When `pump` sees the JVM exit, it posts `{"type":"mode","ask":false}` and returns; the attach loop then fails with `Coderpack exited. Nothing left to run`. This is a shutdown guard, not an observe-only mode.
- Backpressure lives in coderpack's bounded dispatch queue. The Rust `mpsc` channels are unbounded.
- `Router::dropped` counts failed sends to a disconnected JVM writer (any frame) and is never reported. Never treat it as coderpack's event-drop counter.

## Job object

- Only the JVM child is assigned to a kill-on-close job; the game never is.
- `Jvm::spawn` ignores the result of `Job::adopt`, so assignment can fail silently. Never claim job protection is unconditional.
- `Job::create` returns `None` on non-Windows systems and when creation fails; the host then warns and continues.

## Embedded agent

- `build.rs` minifies the agent into `protocol.exe`; `bundle` assembles the injected script in memory. Nothing writes JavaScript to disk.
- The agent source is the first of: `$PROTOCOL_AGENT` (what `launcher/tools/build.ps1` passes); sibling `../coderpack/agent/src`; the `agent.zip` of the coderpack release pinned in `dependencies.json`, cached under `build/agent/<version>/`.
- A sibling without `gen/addr.js` falls back to the pinned release with a `cargo:warning`. `agent::origin()` names the source and the host prints it at startup.
- Keep `js::compact` minimal: strip comments, indentation, blank lines, and repeated spaces only. Never rename, reorder, drop declarations, or join lines. All modules share one scope, so a "smarter" minifier silently loses code, and line breaks carry automatic semicolon insertion. Duplicate helper names overwrite silently; filename order decides the winner.
- `--agent <path>` reads an unminified folder. Keep it an explicit flag, never a search, so a stale `agent` folder from an old install cannot win.
- `--hooks` prints the manifest `build.rs` read from the unminified sources and exits. The launcher relies on it.
- `Config` finds the distribution directory by `zygote.jar` being in it. Never test directory existence; `.` always exists.

## Running

- Module names are agent file names without the numeric prefix. `core`, `bus`, and `names` always load, including under `--only`. Unknown arguments are errors.
- Without `--java`, the host tries `java/bin/java.exe` beside `protocol.exe`, then `%JAVA_HOME%\bin\java.exe`, then `java` on `PATH`.
- `version()` reads `VERSION` beside `protocol.exe` and reports `dev` when it is missing.
- After a crash several candidate processes can exist. `wait_for_game` takes the first match in name-preference order; check the printed name and PID.
- Restart the game between hook-isolation attempts. The agent is not reliably unloaded when only the host exits, and a second host would hook patched code.
- Keep the explicit Frida teardown: unload the script, drop it, sleep 250 ms, then loop. Implicit drop while the target died faulted the host.
- A hook exception without `fileName` arrives as `Message::Other`. Keep `router::other`, which extracts description and stack from the nested JSON.

## Build and test

- Needs the MSVC Rust toolchain and LLVM: `frida-sys` runs bindgen, which needs libclang. `.cargo/config.toml` sets `LIBCLANG_PATH` to `C:\Program Files\LLVM\bin` with `force = false`, so an existing variable wins.
- Commands: `cargo build --release`, `cargo test --release`, `cargo run --example message_check`.
- The `LNK4098` warning about `LIBCMT` is expected (frida-core's static CRT). Never change the CRT setup to silence it.
- Folder-reading bundler tests use the `tests/agent` fixture and never `../coderpack`.
- The two `codec::endtoend` tests pick the newest `api` and `zygote` jars by build time, never by name, from `../coderpack/<part>/build/libs` or `~/.m2/repository/dev/ancaria/coderpack/`. Without both they print `Skipped: No Coderpack JARs found in ../coderpack or ~/.m2` and pass, so a Rust-only checkout needs no JDK.
- `message_check` needs `python` on `PATH` and no game.
- `tools/version.ps1` writes `Cargo.toml` and the matching `Cargo.lock` entry.

## Vendored Frida

- Upstream `frida` 0.17.2 casts callback `user_data` to the wrong handler type for non-`frida:rpc` messages; the first agent `send()` faults the host with `0xC0000005`. `vendor/frida` reads the real handler from `CallbackHandler::script_handler`. `examples/message_check.rs` guards the fix.
- Never edit `vendor/frida` beyond that fix. Return to crates.io only after upstream ships it.
