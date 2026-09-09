# Protocol

## Purpose and process boundary

This repository owns the Sacred Communication Protocol and the Rust host that
carries it. Sacred Gold is a 32-bit process. The mod runtime is a modern 64-bit
JVM, so it must run in a separate `java.exe`. Never move JVM or mod code into the
game process.

The host waits for the game, attaches with Frida, injects the Coderpack
JavaScript agent, starts `dev.ancaria.coderpack.zygote.Main`, and routes traffic
between them. Native game calls and memory access stay inside the injected
agent. Java mods receive events and issue commands across the host boundary.
Hooks exist only in the running game and do not patch game files.

There are two different transports:

1. The injected agent uses Frida messages. Agent-to-host traffic comes through
   `send()`. Host-to-agent traffic uses `Script::post()` with JSON messages.
2. The host and JVM exchange newline-terminated UTF-8 protocol frames through
   the JVM child's anonymous standard I/O pipes. The host writes frames to the
   JVM's stdin and reads frames from its stdout. The agent never talks directly
   to the JVM.

The build produces `target/release/protocol.exe`, which `launcher` packages and
starts. Read `docs/PROTOCOL.md` before changing `src/codec.rs` or
`src/router.rs`. Treat the implementation as authoritative where that document
still describes planned behavior.

## Repository map

| Path | Responsibility |
|---|---|
| `build.rs` | Stages the agent, minifies it, and links it in with the hook manifest |
| `src/main.rs` | Argument parsing, path discovery, game attach loop, the sole Frida poster loop named `pump`, and the verdict watchdog |
| `src/agent.rs` | Builds the single JavaScript source string injected by Frida |
| `src/js.rs` | The minifier and the hook-site reader, shared with `build.rs` |
| `src/codec.rs` | Frame encoding, decoding, percent-encoding, codec tests, and the JVM end-to-end test |
| `src/router.rs` | Converts Frida agent messages to frames, tracks pending asks, and converts JVM frames to work for `pump` |
| `src/jvm.rs` | Starts the Java child and owns separate stdout-reader and stdin-writer threads |
| `src/job.rs` | Windows job-object support and a non-Windows stub |
| `examples/message_check.rs` | Regression check for the vendored Frida callback fix |
| `vendor/frida` | Patched copy of `frida` 0.17.2, documented in `vendor/README.md` |

`Config` identifies the distribution directory by `zygote.jar` being in it. Do
not use directory existence as the test because `.` always exists. There is no
agent directory to discover any more: the modules are inside the executable.

## The embedded agent

`build.rs` reads the agent, minifies each module, and writes the table
`src/agent.rs` links with `include_str!`. `bundle` then assembles the injected
script in memory. Nothing writes JavaScript to the game folder, to a temporary
directory, or anywhere else, and there is no second copy of the agent to go
stale against the binary that loads it.

The source is the first of these that answers:

1. `$PROTOCOL_AGENT`, which is what `launcher/tools/build.ps1` passes when it
   builds this repository from a checkout.
2. The sibling `../coderpack/agent/src`.
3. The `agent.zip` asset of the coderpack release pinned in
   `dependencies.json`, cached under `build/agent/<version>/`.

The third is what makes a lone clone build, and it is why this repository has a
`dependencies.json` at all: coderpack generates `gen/addr.js` rather than
committing it, so a checkout is not always enough and a release asset always is.
A sibling that has the modules but no `gen/addr.js` falls back to the pinned
release with a `cargo:warning` saying so, because building somebody's edited
agent out of a release without a word is how a change appears not to have taken.
`agent::origin()` names the source, and the host prints it at startup.

Minification is `js::compact`, and it is deliberately the smallest thing that
counts as one: comments, indentation and blank lines out, runs of spaces
collapsed, everything else untouched. No renaming, no reordering, no dropping of
a declaration, and no joining of lines. Every module shares one scope, so a
minifier that decided a top-level function was unused would produce a bundle
that loads and silently does less. Line breaks stay because automatic semicolon
insertion is part of the language. No source map is produced or shipped.

`--agent <path>` reads a folder instead, unminified, which is what somebody
editing the agent wants: change a file, restart the host, no rebuild. It is an
explicit flag and not a search: a stale `agent` folder left in a game folder by
an older install must never quietly win over the agent in the binary.

`--hooks` prints the hook manifest as JSON and exits before anything is started.
`build.rs` reads it out of the sources before they are minified, so the names in
it are the names in the checkout. The launcher asks the `protocol.exe` in the
game folder for it, because that is what knows which agent is in it.

## Wire invariants

The JVM wire is flat text with one UTF-8 frame per line. The frame forms are
defined in `docs/PROTOCOL.md`:

```text
EVT <seq> <type> <k>=<v> ...
ASK <seq> <type> <k>=<v> ...
END <seq> ok=1
END <seq> cancel=1
END <seq> set.<k>=<v> ...
CMD <seq> <name> <k>=<v> ...
RES <seq> ok=1 [<k>=<v> ...]
RES <seq> err=<text>
BYE
```

`EVT` reports an observation and expects no verdict. `ASK` is valid only when
the agent can still cancel or rewrite the pending game operation. `END` has no
name token. `CMD` travels from the JVM toward the game and receives `RES`.
The current agent uses sequence `0` for `EVT` and a session-local increasing
counter for `ASK`. Coderpack has a separate increasing counter for `CMD`.

`docs/PROTOCOL.md` still lists `LOG <level> <text>`, but the Rust and Java frame
decoders require the second token of every non-`BYE` line to be a decimal
sequence number. That documented `LOG` form is not valid on the JVM wire.
Agent logs arrive as Frida `log` payloads, and Coderpack logs go to stderr.
If wire-level `LOG` frames are retained, specify and test one form on both
sides before relying on them.

Keys and values percent-encode only `%`, space, `=`, LF, and CR as `%25`,
`%20`, `%3D`, `%0A`, and `%0D`. Fields must remain flat. If `flatten` receives
an object or array, it logs a warning and serializes that value into one field.
Coderpack cannot look through that JSON blob, so nested output is a protocol
bug.

The JVM's stdout carries frames only. `System.out` from a mod corrupts the
wire. `jvm.rs` reports such a line as
`[coderpack] Could not parse Coderpack output: <line>`. JVM stderr is inherited
and is the correct path for Java diagnostics. Mods should use the loader's own
logging API.

`BYE` is specified as host-to-Coderpack shutdown. The current router also
accepts `BYE` from Coderpack and posts `{"type":"mode","ask":false}` to the
agent. Keep this compatibility behavior unless both sides and the protocol
specification change together.

## Concurrency, verdicts, and backpressure

Nothing in `router.rs` may wait for JVM work or a command result. An `ASK`
blocks the game thread in Frida's `recv().wait()`. A mod that calls back into
the game from an event handler will deadlock if frame reading and mod dispatch
share a thread.

Keep the existing ownership split:

- Frida's callback converts agent messages and sends encoded frames to the JVM
  writer channel.
- The JVM stdout reader decodes `END`, `CMD`, `LOG`, and `BYE`, then enqueues
  work for `pump`.
- Only `pump` owns the Frida `Script` and calls `Script::post()`.
- Coderpack's stdin reader stays separate from its mod-dispatch thread so
  command replies can complete while a mod handler waits.

`VERDICT_DEADLINE` is 250 ms. The watchdog scans every 125 ms and considers an
ask overdue only after its age is greater than 250 ms. The fallback can
therefore be queued roughly 250 to 375 ms after the ask, plus scheduler and
poster-loop delay. Do not describe 250 ms as a strict worst-case block time.
When an ask is overdue, the host posts an allow verdict with no rewrites and
logs `[host] Coderpack did not answer request <seq> in time. Continuing`.
The slow mod loses its veto for that ask.

When `pump` notices that the JVM has exited, it posts
`{"type":"mode","ask":false}` so the attached agent cannot wait for another
verdict. `pump` then returns. The attach loop subsequently fails with
`Coderpack exited. Nothing left to run`. This is a shutdown safeguard, not a
long-running observe-only mode. `pump` checks detach and JVM state on a 200 ms
receive timeout.

Backpressure is bounded inside Coderpack, not in the Rust host. Coderpack's
dispatch queue holds 4096 frames. `ASK` uses a blocking `put` and is never
dropped there. Non-`ASK` dispatch frames use `offer`. In normal traffic these
are events. They are dropped when the queue is full and reported by Coderpack
after the first drop and every hundredth drop. `RES` completes a command
before that queue, and `BYE` takes the shutdown path. The Rust channels created
with `std::sync::mpsc::channel` are unbounded.

`Router::dropped` has a narrower and currently misleading role. It increments
when the channel to the JVM writer is disconnected. That failed send can be an
`EVT`, `ASK`, or `RES`, and the count is never reported. Do not treat it as the
Coderpack queue's event-drop counter. If an `ASK` send fails after being added
to `Pending`, the watchdog eventually posts the allow verdict.

## Job-object behavior

On Windows, `Job::create` creates a job and configures
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. `Jvm::spawn` then attempts to assign only
the JVM child to that job. The game process is never assigned, and the job does
not restart anything.

The JVM is killed with the host only when job creation, limit configuration,
and process assignment all succeed. The current code checks creation and
configuration, but it ignores the boolean returned by `Job::adopt`. Assignment
failure is therefore silent and removes the kill-on-close guarantee. Do not
claim that job protection is unconditional. If `Job::create` returns `None`,
the host warns
`[host] Couldn't create a job object. The JVM may keep running if the host crashes`
and continues. On non-Windows systems, `Job::create` always returns `None`.

## Build and test

Use Rust 1.98 with the MSVC toolchain and install LLVM. The crate uses edition
2024, whose compiler floor is Rust 1.85. `frida-sys` runs bindgen, which needs
libclang. `.cargo/config.toml` supplies
`C:\Program Files\LLVM\bin` as `LIBCLANG_PATH` with `force = false`, so an
existing environment variable wins.

```text
cargo build --release
cargo test --release
cargo run --example message_check
```

`cargo build --release` writes `target/release/protocol.exe`.
`cargo test --release` currently runs nineteen tests. The folder-reading bundler
tests use this repository's `tests/agent` fixture, including its generated
address table, and never read `../coderpack`. The rest check the agent that was
linked in, whichever of the three sources it came from: that it loads in the
right order, that it kept `core`, `bus` and `names`, that it arrived minified,
and that the hook manifest names modules and sites.

`codec::endtoend::zygote_answers_every_ask` starts a real Coderpack JVM with no
mods. It selects the lexically newest `api` and `zygote` JARs it can find under
`../coderpack/<part>/build/libs` or
`~/.m2/repository/dev/ancaria/coderpack/`. If either JAR is missing, it prints
`Skipped: No Coderpack JARs found in ../coderpack or ~/.m2` and passes. A
Rust-only checkout therefore needs no JDK. A real host run requires JDK 21 or
newer.

`message_check` needs `python` on `PATH` and no game. It attaches to a
throwaway Python process, waits up to 5 seconds for one Frida `send()`, unloads
the script, and kills the child. The child itself has a 20-second sleep as a
safety margin.

The `LNK4098` warning about `LIBCMT` comes from frida-core's static CRT
conflicting with Rust's dynamic CRT. It is expected for this build. Do not try
to fix it by changing the CRT setup.

CI runs on Windows for pushes to `master`, pull requests, and manual dispatch.
It installs LLVM, uses the stable Rust toolchain, runs `cargo fmt --check` as a
non-blocking check, then runs the release build and release tests. Every run
uploads `protocol.exe` as a workflow artifact.

On a push to `master`, CI reads `version` from `Cargo.toml`. If the remote has
no `v<version>` tag, the release step creates that tag and uploads
`target/release/protocol.exe` as the release asset. Raising the version is what
ships a release. The launcher downloads this asset when no sibling protocol
checkout is available. `tools/version.ps1` prints the current version with no
argument, or raises it in `Cargo.toml` and the matching `Cargo.lock` entry with
`pwsh tools/version.ps1 0.99.1`.

## Running and path discovery

The launcher normally starts the host before it starts the game. From an
installed layout, the host can also run with no arguments:

```text
<Sacred Gold>\launcher\protocol.exe
```

The agent comes from inside the executable unless `--agent` says otherwise.
Distribution candidates are the executable directory, `dist`, and `.`, selected
by the presence of `zygote.jar`. Installed mods live at
`<Sacred Gold>\mods`, one level above `<Sacred Gold>\launcher`.

Without `--java`, the host first checks `java/bin/java.exe` beside
`protocol.exe`, then `%JAVA_HOME%\bin\java.exe`, then `java` on `PATH`. The
launcher installs downloaded Java under
`<Sacred Gold>\launcher\java` and normally passes that executable through
`--java`.

| Flag | Effect |
|---|---|
| `--skip gold,position` | Leaves those agent modules out of the bundle |
| `--only health` | Loads only the named optional module |
| `--no-hook goldEpilogue` | Keeps the module but skips that attach site |
| `--trace` | Logs each hook when it fires |
| `--no-ask` | Installs hooks but disables verdict waits |
| `--hooks` | Prints the hook manifest as JSON and exits |
| `--agent <path>` | Reads the agent from a directory instead of the built-in one |
| `--dist <path>` | Overrides the JAR directory |
| `--mods <path>` | Overrides the mods directory |
| `--java <path>` | Overrides the Java executable |
| `--enable <ids>` | Loads the comma-separated mod IDs |

Module names are agent file names without the numeric prefix. `core`, `bus`,
and `names` always load, including under `--only`. Agent JavaScript files load
in lexical filename order after `gen/addr.js`. Unknown arguments are errors.

## Cross-repository build boundary

- `../coderpack` owns the injected JavaScript under `agent/src`, plus
  `api.jar`, `zygote.jar`, and
  `dev.ancaria.coderpack.zygote.Main`. A build of this repository does not
  require that checkout, but it does require that repository's agent: without a
  checkout it downloads the `agent.zip` of the release pinned in
  `dependencies.json`. A normal host run additionally requires the JAR outputs.
- `../mappings` owns every game address. This repository never reads mappings
  directly. Coderpack's `python tools/addr.py` generates
  `agent/src/gen/addr.js`, which is embedded here as the first module.
- `../launcher` runs `cargo build --release` when this checkout is present,
  with `PROTOCOL_AGENT` pointing at its coderpack sibling, then stages
  `protocol.exe`, both JARs, and `VERSION`. It no longer stages an agent
  folder. Without this checkout it downloads the pinned protocol release, whose
  agent is the one that repository's build embedded.

The launcher starts the host with `--enable` and usually `--java`. It adds
`--no-hook` for disabled sites, starts the host before the game, waits for the
game to exit, allows 500 ms for clean Frida detach, then kills the host.
`version()` reads `VERSION` beside `protocol.exe` and reports `dev` when the
file is missing.

The address table targets `pureHD.exe` v2.0.2.118. The host still searches for
`pureHD.exe`, `Sacred.exe`, and `Game.exe` in that preference order, comparing
names without regard to case. Detection does not prove binary compatibility.
Never add an address here or assume another executable uses the same RVAs.

## Failure modes and warnings

- Run the host as Administrator when the game is elevated. Frida cannot attach
  across the privilege boundary. The host may keep waiting even though process
  enumeration can see the game.
- More than one candidate process can survive a crash. `wait_for_game` checks
  names in preference order and takes the first matching enumerated process
  for that name. Confirm the printed executable name and PID.
- Restart the game between hook-isolation attempts. An injected agent is not
  reliably unloaded when only the host exits. A second host can try to hook
  instructions that are already patched. Coderpack's `10-core.js` refuses
  known duplicate patches by checking for `0xE9` or `0xCC` at `hpDamage` and
  `goldDelta`.
- Keep the explicit Frida teardown order. `main` unloads the script, drops it,
  and sleeps 250 ms before looping. Implicitly dropping a script while the
  target was dying previously faulted the host.
- `END` frames have no name. `END 3 cancel=1` once parsed `cancel=1` as a name
  and discarded the verdict fields. Cancellation and rewrites then failed
  silently. Keep `codec::tests::verdicts_keep_their_fields`.
- Hook exceptions without `fileName` arrive as `Message::Other`, not
  `Message::Error`. Keep `router::other`, which extracts the description and
  stack from the nested JSON.
- The bundled agent is one JavaScript scope. Duplicate helper names overwrite
  one another silently. Filename order decides which definition wins.
- An agent change now reaches players through a release of this repository. The
  launcher no longer ships the scripts, so a coderpack release alone changes
  nothing in a player's game: release coderpack, raise the pin in
  `dependencies.json` if this repository builds from it, and release this one.
  A workspace build takes the sibling and shows the change immediately, which
  is exactly the case that can hide the missing release.
- Upstream `frida` 0.17.2 casts callback `user_data` to the caller's handler
  type for non-`frida:rpc` messages even though it points to
  `CallbackHandler`. The first agent `send()` then faults the host with
  `0xC0000005`, module `unknown`. The vendored patch reads the actual handler
  from `CallbackHandler::script_handler`.
- Do not edit `vendor/frida` beyond that callback fix. Remove the vendored copy
  and return to the crates.io dependency only after upstream includes the fix.
- Never hand-write a game address in this repository. Regenerate
  `gen/addr.js` from mappings. A stale table can attach to the wrong code or
  produce hooks that never fire without a useful error.
