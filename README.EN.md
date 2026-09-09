<div align="center">

[![Rust](https://img.shields.io/badge/Rust-1.98-CE422B?style=for-the-badge&logo=rust&logoColor=white&labelColor=1C1410)](https://www.rust-lang.org)
[![Frida](https://img.shields.io/badge/Frida-0.17.2-3B5998?style=for-the-badge&labelColor=1C1410)](https://frida.re)
[![License](https://img.shields.io/badge/License-MIT-4B5563?style=for-the-badge&labelColor=1C1410)](LICENSE)
[![Sacred Community](https://img.shields.io/badge/Sacred-Community-8B1A1A?style=for-the-badge&labelColor=1C1410)](https://ancaria.dev)

</div>

[Русский](README.md) · [Deutsch](README.DE.md)

# Sacred communication protocol

Sacred Gold is a 32-bit game from 2004. The mod loader uses a 64-bit JVM, so
the two cannot share a process. This repository contains the line protocol
between them and the Windows host that carries it.

Mods run in a separate `java.exe`, while Frida injects the JavaScript agent
into the game. Agent and host exchange Frida messages. Host and JVM exchange
newline-terminated UTF-8 frames through the JVM's standard I/O pipes. The host
writes to JVM stdin and reads from JVM stdout. The agent never talks to the JVM
directly. [docs/PROTOCOL.md](docs/PROTOCOL.md) defines the frame format.

The executable waits for the game, injects the agent from the
[coderpack](https://github.com/ancaria-dev/coderpack) repository, starts
`dev.ancaria.coderpack.zygote.Main` with `api.jar` and `zygote.jar` on its
classpath, and passes messages in both directions. It writes nothing to the game's files. The hooks live in memory and
disappear when the game process exits.

The agent's JavaScript is inside this executable: the modules are minified and
linked in at build time, and the script Frida injects is assembled in memory
from them. No JavaScript is written to the game folder, to a temporary
directory, or anywhere else. `--agent <path>` reads a folder instead, which is
what somebody editing the agent wants.

## Running it

The launcher normally starts the host. You can also run it directly from the
installed folder:

```
<Sacred Gold>\launcher\protocol.exe
```

Run it elevated if the game is elevated. Frida cannot attach across that line,
and the host will keep waiting even though it can see the process. Paths
resolve beside the executable, so no arguments are normally required.

## What the wire looks like

A session can contain these three frames:

```
EVT 2 hero.captured class=9 className=Daemon level=142 hp=27127 maxHp=27127
ASK 3 health.damage entity=player damage=553 current=19849 next=19296 max=26999
END 3 set.next=19849
```

`EVT` reports an event and expects no reply. `ASK` comes from a hook placed
before a write, and the game thread stops while it waits. `END` supplies the
answer. It can allow the write, cancel it, or replace fields. In this example,
the replacement undoes the damage before the game commits it. A mod can also
send a command on its own:

```
CMD 1 player.gold
RES 1 ok=1 gold=104233
```

`LOG` and `BYE` complete the frame set. Keys and values percent-encode only the
characters that would break the format: `%` as `%25`, space as `%20`, `=` as
`%3D`, LF as `%0A`, and CR as `%0D`. Every other character remains literal,
which keeps live sessions readable in a terminal.

Because `ASK` blocks the game thread, three rules apply:

- **A verdict has a deadline.** The watchdog checks every 125 ms and treats an
  `ASK` as overdue once it is older than 250 ms. It then queues `END ok=1` and
  logs the sequence number. The fallback is usually queued 250 to 375 ms after
  the request, plus scheduling and poster-loop delay. A slow mod loses its veto
  for that request. If the JVM exits, the host disables further `ASK` waits
  before shutting down.
- **Whatever answers an `ASK` must not read replies on the same thread.** A mod
  calling back into the game from inside an event handler deadlocks it. Both
  sides split reading from dispatch for that reason.
- **stdout carries frames and nothing else.** Logs go to stderr. A mod that
  prints with `System.out` corrupts the stream, and the host reports that line
  as unparseable Coderpack output. The loader provides its own logging API.

On Windows, the host tries to place the JVM in a job object configured with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The game process never joins that job.
When job creation, limit configuration, and process assignment all succeed,
closing the host kills the JVM. A creation or configuration failure produces a
warning and the host continues. Process assignment failure is currently silent,
so the kill-on-close guarantee is conditional.

## When the game crashes

These flags isolate a failing hook without requiring a rebuild. Restart the
game between attempts. An injected agent is not reliably unloaded when only
the host exits, so another host can try to patch an already hooked instruction.

| Flag | What it does |
|---|---|
| `--skip gold,position` | leave those agent modules out |
| `--only health` | load nothing else (`core`, `bus`, and `names` always load) |
| `--no-hook goldEpilogue` | keep the module, skip that one attach site |
| `--trace` | every hook announces itself as it runs |
| `--no-ask` | hooks stay installed, verdicts are disabled |
| `--agent <path>` | read the agent out of a folder instead of the built-in one |
| `--hooks` | print the hook sites as JSON and stop |

Module names are the agent's file names without the number prefix. `--agent`,
`--dist`, `--mods`, and `--java` override the paths that the host discovers.
`--hooks` is how the launcher draws its list of switchable sites: it asks the
copy of the host in the game folder, because that is what knows which agent is
in it.
`--enable` selects the mod IDs to load. Without `--java`, the host first checks
for `java/bin/java.exe` beside the executable, where the launcher installs a
downloaded JDK. It then checks `%JAVA_HOME%\bin\java.exe` before using `java`
on `PATH`. The launcher normally passes its downloaded Java through `--java`.
A real host run requires JDK 21 or newer. Unknown arguments stop startup with
an error.

## Building

```
cargo build --release
```

The build needs the agent, and takes it from the first of these it finds:
`$PROTOCOL_AGENT`, the sibling `../coderpack/agent/src`, or the `agent.zip`
asset of the coderpack release pinned in `dependencies.json`, which it caches
under `build/agent/`. The third is what makes a lone clone build: coderpack
generates its address table rather than committing it, so a checkout is not
always enough and a release asset always is. The host prints which one it was
built from as it starts.

The result is `target/release/protocol.exe`. You need Rust 1.98 with the MSVC
toolchain and an LLVM install, because `frida-sys` runs bindgen and bindgen
needs libclang. `.cargo/config.toml` points `LIBCLANG_PATH` at
`C:\Program Files\LLVM\bin`, but an existing environment variable takes
precedence. The `LNK4098` link warning comes from frida-core's static CRT
conflicting with Rust's dynamic CRT. It is expected.

`vendor/frida` is a patched copy of the `frida` crate 0.17.2. For every message
that is not `frida:rpc`, upstream casts the callback's `user_data` to the
caller's handler type even though it is a different type. As a result, the
first `send()` from an agent faulted the host with `0xC0000005`, and no message
arrived. `vendor/README.md` documents the patch. This command needs `python` on
`PATH`, attaches to a throwaway process, and waits up to 5 seconds for a
message. It does not need the game:

```
cargo run --example message_check
```

`cargo test --release` currently runs nineteen tests covering the frame codec,
the minifier, the agent bundler, and the JVM boundary. The folder-reading
bundler tests use the repository's fixture agent in `tests/agent`, so they never
read `../coderpack`; the rest check the agent that was linked in, whichever of
the three sources it came from. The
end-to-end test starts a real Coderpack JVM with no mods and checks that every
`ASK` receives a frame the host can parse. It uses the lexically newest `api`
and `zygote` JARs it can find in a coderpack checkout beside this repository or
in the local Maven repository
(`~/.m2/repository/dev/ancaria/coderpack`). If neither location contains the
jars, the test prints a skip message and passes. A Rust-only checkout therefore
needs no JDK.

CI runs on Windows for pushes to `master`, pull requests, and manual dispatch.
It treats `cargo fmt --check` as non-blocking, then runs the release build and
release tests. Every run uploads `protocol.exe` as a workflow artifact. On a
successful push to `master`, CI reads `version` from `Cargo.toml`. If the
remote has no `v<version>` tag, the release step creates that tag and a release
containing `target/release/protocol.exe`. A version change ships only after
that publishing step succeeds. When the launcher builds without a sibling
protocol checkout, it downloads this release asset.

## License

The project uses the MIT License. See [LICENSE](LICENSE).

