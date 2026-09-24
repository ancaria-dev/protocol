<div align="center">

[![Rust](https://img.shields.io/badge/Rust-1.98-CE422B?style=for-the-badge&logo=rust&logoColor=white&labelColor=1C1410)](https://www.rust-lang.org)
[![Frida](https://img.shields.io/badge/Frida-0.17.2-3B5998?style=for-the-badge&labelColor=1C1410)](https://frida.re)
[![License](https://img.shields.io/badge/License-MIT-4B5563?style=for-the-badge&labelColor=1C1410)](LICENSE)
[![Sacred Community](https://img.shields.io/badge/Sacred-Community-8B1A1A?style=for-the-badge&labelColor=1C1410)](https://ancaria.dev)

</div>

[Русский](README.md) · [Deutsch](README.DE.md)

# Sacred Communication Protocol

The host that connects Sacred Gold to the mods running in Java. You need this
repository if you work on the loader itself, not to write a mod.

Sacred Gold is a 32-bit game, and the loader's JVM is 64-bit, so they can't
share a process. `protocol.exe` waits for the game, injects the JavaScript
agent from [coderpack](https://github.com/ancaria-dev/coderpack) with Frida,
and starts the JVM in a separate `java.exe`. Then it carries messages both
ways.

The agent talks to the host through Frida messages. The host talks to the JVM
in text frames, one UTF-8 line each, over two named pipes: `<base>.in` toward
the JVM and `<base>.out` back. The agent never talks to the JVM directly.
[docs/PROTOCOL.md](docs/PROTOCOL.md) defines the frames.

The agent's JavaScript is built into `protocol.exe`. The host assembles the
injected script in memory and writes nothing to the game folder or anywhere
else. The hooks live only in the game's memory and disappear when it exits.

## Getting started

The launcher normally starts the host. You can also run it by hand from the
installed folder:

```
<Sacred Gold>\launcher\protocol.exe
```

It finds its paths next to the executable, so it needs no arguments. If the
game runs as administrator, run the host as administrator too. Otherwise Frida
can't attach, and the host keeps waiting even though it sees the process.

The host looks for Java in this order: `java/bin/java.exe` next to itself
(where the launcher unpacks a downloaded JDK), `%JAVA_HOME%\bin\java.exe`,
then `java` on `PATH`. The launcher usually passes its Java with `--java`. A
real run needs JDK 21 or newer.

| Flag | What it does |
|---|---|
| `--enable <ids>` | Loads the mods with these comma-separated IDs |
| `--java <path>` | Uses this Java executable |
| `--dist <path>` | Reads the JARs from this folder |
| `--mods <path>` | Reads mods from this folder |
| `--agent <path>` | Reads the agent from a folder instead of the built-in one, handy while you edit it |
| `--hooks` | Prints the hook sites as JSON and exits |

An unknown argument stops the host with an error.

## Frames

Here's part of a session:

```
EVT 2 hero.captured class=9 className=Daemon level=142 hp=27127 maxHp=27127
ASK 3 health.damage entity=player damage=553 current=19849 next=19296 max=26999
END 3 set.next=19849
```

`EVT` reports an event and expects no answer. `ASK` comes from a hook placed
before a write, and the game thread stops until it gets an answer. `END` is
that answer: it allows the write, cancels it, or replaces fields. Here
`set.next=19849` keeps the old health, so the damage never lands.

A mod can also send a command on its own:

```
CMD 1 player.gold
RES 1 ok=1 gold=104233
```

`BYE` ends the session. Keys and values percent-encode only the characters
that would break a frame: `%`, space, `=`, LF, and CR. Everything else stays
literal, so you can read a live session in a terminal.

### Rules for an `ASK`

An `ASK` holds the game thread, so three rules apply:

- **An answer has a deadline.** The watchdog checks every 125 ms and marks an
  `ASK` overdue once it's older than 250 ms. The host then answers `END ok=1`
  for the mod and logs the sequence number. That usually happens 250–375 ms
  after the request, plus scheduler and poster-loop delay. The slow mod loses
  its veto for that request. If the JVM exits, the host stops waiting for
  answers and shuts down.
- **Don't read replies on the thread that answers.** A mod that calls back into
  the game from an event handler would deadlock it. Both sides keep reading
  and dispatch on separate threads for that reason.
- **Frames have their own channel.** A mod that prints with `System.out`
  breaks nothing: its output lands in the host console. The host reports a
  line it can't parse as unreadable Coderpack output. For logging, use the
  loader's API, which prefixes each line with the mod ID.

### When the host exits

On Windows the host tries to put the JVM in a job object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the JVM dies with the host. The game
never joins that job. The guarantee holds only if creating the job, setting
its limit, and assigning the JVM all succeed. The first two failures print a
warning. An assignment failure is silent for now.

## When the game crashes

These flags isolate a failing hook without a rebuild:

| Flag | What it does |
|---|---|
| `--skip gold,position` | Leaves these agent modules out |
| `--only health` | Loads only this module. `core`, `bus`, and `names` always load |
| `--no-hook goldEpilogue` | Keeps the module but skips this one site |
| `--trace` | Logs every hook as it fires |
| `--no-ask` | Keeps the hooks but never waits for an answer |

A module name is the agent file name without its number prefix. Restart the
game between attempts. The injected agent may stay in the game after the host
exits, and a new host would then patch an instruction that's already hooked.

The launcher builds its list of switchable sites from `--hooks`. It asks the
`protocol.exe` in the game folder, because only that copy knows which agent is
inside it.

## Building

You need Rust 1.98 with the MSVC toolchain and LLVM: `frida-sys` runs bindgen,
and bindgen needs libclang. `.cargo/config.toml` points `LIBCLANG_PATH` at
`C:\Program Files\LLVM\bin` unless the variable is already set.

```
cargo build --release
cargo test --release
```

The result is `target/release/protocol.exe`. The `LNK4098` link warning comes
from frida-core's static CRT meeting Rust's dynamic one. It's expected.

The build takes the agent from the first source it finds:

1. `$PROTOCOL_AGENT`
2. The sibling `../coderpack/agent/src`
3. The `agent.zip` of the coderpack release pinned in `dependencies.json`,
   cached under `build/agent/`

The third source is what lets a lone clone build. coderpack generates its
address table instead of committing it, so a checkout isn't always enough, but
a release asset is. The host prints its agent's source at startup.

The tests cover the frame codec, the minifier, the agent bundler, and the JVM
boundary. Two end-to-end tests start a real Coderpack JVM with no mods and
check that every `ASK` gets a frame the host can parse. They take the most
recently built `api` and `zygote` JARs from a coderpack checkout next to this
one or from `~/.m2/repository/dev/ancaria/coderpack`. Without them, the tests
print a skip message and pass, so a Rust-only checkout needs no JDK.

`vendor/frida` is a patched copy of the `frida` crate 0.17.2. Upstream casts a
callback's `user_data` to the wrong type for every message except
`frida:rpc`, and the agent's first `send()` crashed the host with
`0xC0000005`. `vendor/README.md` describes the patch. This check needs
`python` on `PATH` but not the game:

```
cargo run --example message_check
```

## Releases

CI runs on Windows for every push to `master`, every pull request, and on
demand. It builds and tests in release mode and uploads `protocol.exe` as a
workflow artifact. `cargo fmt --check` runs too but doesn't fail the build.

On `master`, CI reads `version` from `Cargo.toml`. If the `v<version>` tag
doesn't exist yet, CI creates it and publishes a release with `protocol.exe`.
To release, raise the version with `pwsh tools/version.ps1 <version>`. The
launcher downloads this asset when it builds without a protocol checkout next
to it.

An agent change reaches players only through a release of this repository.
[CONTRIBUTING](https://github.com/ancaria-dev/.github/blob/master/CONTRIBUTING.EN.md)
lists the release order across the whole loader.

## License

MIT, see [LICENSE](LICENSE).
