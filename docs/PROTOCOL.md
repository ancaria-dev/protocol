# Sacred Communication Protocol (v0)

One line per message, UTF-8, `\n`-terminated. Deliberately not JSON: the JDK
ships no JSON parser, and a flat line stays readable in a terminal — which is
most of what this layer does early on.

```
agent (Frida, inside the game)
        │  frida message
        ▼
protocol.exe (Rust)  ──stdout──►  java.exe: zygote + mods
                     ◄──stdin───
```

The host owns both ends. The agent never talks to the JVM directly.

## Frames

| Frame | Direction | Meaning |
|---|---|---|
| `EVT <seq> <type> <k>=<v> ...` | host → Coderpack | happened, no answer expected |
| `ASK <seq> <type> <k>=<v> ...` | host → Coderpack | about to happen, answer required |
| `END <seq> ok=1` | Coderpack → host | let it proceed unchanged |
| `END <seq> cancel=1` | Coderpack → host | suppress the write |
| `END <seq> set.<k>=<v> ...` | Coderpack → host | rewrite these fields, then proceed |
| `CMD <seq> <name> <k>=<v> ...` | Coderpack → host | do something / read something |
| `RES <seq> ok=1 [<k>=<v> ...]` | host → Coderpack | command result |
| `RES <seq> err=<text>` | host → Coderpack | command failed |
| `LOG <level> <text>` | both | diagnostics |
| `BYE` | host → Coderpack | shutting down |

`seq` is decimal, monotonic, unique per session. `EVT`/`ASK` and `CMD` draw
from separate counters — they flow in opposite directions.

Values are percent-encoded only for bytes that would break the format:
`%20` space, `%3D` `=`, `%0A` newline, `%25` `%`. Everything else is literal.

## Which events may be asked

`ASK` is valid only where the hook sits **before** the write and owns a
register we can rewrite. That is a property of the game's code, not a design
choice:

| Event | Frame | Why |
|---|---|---|
| `health.damage` | `ASK` | `+0x16FC44` commits EAX, which we own |
| `exp.gain` | `ASK` | `+0x17EAE4` commits EAX, gain in ESI |
| `gold.delta` | `ASK` | the AddGold entry owns the delta argument |
| `item.pickup` | `ASK` | the pickup function reads the item from its `ref` argument |
| `skill.change` | `ASK` | `+0x1827DA` commits the low byte of ECX |
| `attr.spend` | `ASK` | no register to swap, see below |
| `health.death` | `EVT` | the source register is a zero constant the path reuses |
| `entity.*` | `EVT` | observation only, no rewrite point identified |
| `item.stored` / `item.equip` / `item.moved` | `EVT` | no clean no-op path: on equip, `ref` 0 means *unequip*, not *do nothing* |
| `level.changed` | `EVT` | the level is anti-cheat mirrored and drives grant tables |
| every `*.changed` | `EVT` | already committed |

A verdict does not have to be a register swap. `attr.spend` is asked *after*
the game's own grant returns, and the agent applies the answer by writing the
field. Nothing else has observed it yet, so the result is the same. What makes
an event askable is that Coderpack's answer can still decide the value, not the
particular way it is applied.

`item.pickup` is the clearest case of that. The game's pickup function resolves
its `ref` argument through the object table and, when the lookup fails, jumps to
its own epilogue having done nothing. So `cancel` writes 0 into that argument,
the game's own "nothing there" path, and `set.ref` writes a different one,
which makes the same code pick up a different object. Only the hero's pickups
are asked about. A creature picking something up arrives as `EVT` with
`player=0`, because the rate on those three call sites has never been measured
and an `ASK` stops the game thread.

The verdict carries a second, different power. `set.ref` chooses **which**
object is picked up and lasts for that one call. `set.type` (and `set.level`,
`set.min`, `set.atk`, `set.pct`) rewrite the object **itself**, and outlive the
event: drop the item again and it is still what it was made into. They compose
in that order: the ref picks the object, the rest reshape whatever was picked.

`set.type` is the item's **label**, not its identity. Confirmed in game, it
renames and redraws and changes nothing else. What an item *does* is `set.mods`,
the modifier list (`id:value` pairs), which is why a retyped rune keeps
upgrading the art it always did. Writing modifiers is implemented and **not yet
verified against the game**. The display *name* cannot be written at all:
Sacred composes it from affixes at draw time.

Sending `ASK` for anything else is a bug: Coderpack answers `END ok=1` and logs it
rather than pretend the verdict mattered.

## Commands

`CMD` is how a mod acts on its own initiative. The host turns it into a
`NativeFunction` call or a memory write inside the agent, on the game's
thread, and answers with `RES`.

```
CMD 7 player.teleport x=222850 y=137608
RES 7 ok=1
CMD 8 player.gold value=104233
RES 8 ok=1 gold=104233
```

Implemented so far: `player.state`, `player.teleport`, `player.hp`,
`player.exp`, `player.gold`, `ui.string`, `type.name`, `type.find`,
`type.list`, `item.info`, `item.reshape`.

`RES` fields are **flat**, like every other frame. Nesting them looks like it
works and does not: the host turns an inner object into one field holding JSON,
so Coderpack looks a value up by name and finds nothing, with no error anywhere. That
is how `ui.string` returned null for its whole life.

`type.find` and `type.list` are the reverse of `type.name`, which the game has
no primitive for, so the agent builds the index by asking `typeName` for every id
once, on first use. A mod should always name a type rather than write its id:
ids are build-specific numbers with no meaning, and
`TYPE_OBJECT_POTION_LARGE_RED` survives a build that renumbers them.

```
CMD 3 type.list prefix=TYPE_OBJECT_POTION_
RES 3 ok=1 n=24 types=TYPE_OBJECT_POTION_SMALL_RED:5171,...
CMD 4 item.reshape ref=1083 type=5100
RES 4 ok=1 ref=1083 type=5100 name=TYPE_OBJECT_POTION_LARGE_RED
```

A command naming a live object takes its object-manager `ref`, never a raw
address — heap addresses move every launch.

## Timing

`ASK` blocks the game thread until the verdict arrives, so it is used only at
sites measured cold. From this project's measurements: the damage writer peaks
at ~5 calls/s in real combat, a rune invest is one call per click, a kill is
one call per mob. The regen writer (~10/s per creature) is never exposed.

Frida's `recv().wait()` has no timeout, so the timeout lives in the host: if
Coderpack does not answer within the deadline the host replies `END ok=1` itself and
marks the mod slow. The agent always unblocks. **The agent trusts the host;
the host trusts nothing.**

## Backpressure

The `EVT` queue is bounded. If Coderpack falls behind, `EVT` frames are dropped and
counted (the count is reported as a `LOG`). `ASK` frames are never dropped.

## Example

```
EVT 1 session.world_loaded
EVT 2 hero.captured class=9 className=Daemon level=142 hp=27127 maxHp=27127
ASK 3 health.damage entity=player damage=553 current=19849 next=19296 max=26999
END 3 set.next=19849
EVT 4 entity.death type=TYPE_NPC_SPIDER_M2 level=42 ref=903
ASK 5 item.pickup ref=8814 name=TYPE_OBJECT_RING_FIRE01 level=30 player=1
END 5 set.ref=8901
CMD 1 player.gold
RES 1 ok=1 gold=104233
```
