<div align="center">

[![Rust](https://img.shields.io/badge/Rust-1.98-CE422B?style=for-the-badge&logo=rust&logoColor=white&labelColor=1C1410)](https://www.rust-lang.org)
[![Frida](https://img.shields.io/badge/Frida-0.17.2-3B5998?style=for-the-badge&labelColor=1C1410)](https://frida.re)
[![License](https://img.shields.io/badge/License-MIT-4B5563?style=for-the-badge&labelColor=1C1410)](LICENSE)
[![Sacred Community](https://img.shields.io/badge/Sacred-Community-8B1A1A?style=for-the-badge&labelColor=1C1410)](https://ancaria.dev)

</div>

[Русский](README.md) · [English](README.EN.md)

# Sacred Communication Protocol

Der Host, der Sacred Gold mit den Mods in Java verbindet. Dieses Repository
brauchst du, wenn du am Loader selbst arbeitest. Für einen Mod brauchst du es
nicht.

Sacred Gold ist ein 32-Bit-Spiel, die JVM des Loaders läuft mit 64 Bit. In
einem Prozess vertragen sie sich nicht. `protocol.exe` wartet auf das Spiel,
schleust mit Frida den JavaScript-Agenten aus
[coderpack](https://github.com/ancaria-dev/coderpack) ein und startet die JVM
in einem eigenen `java.exe`. Danach reicht es Nachrichten in beide Richtungen
weiter.

Der Agent spricht mit dem Host über Frida-Nachrichten. Der Host spricht mit der
JVM in Text-Frames, je eine UTF-8-Zeile, über zwei Named Pipes: `<base>.in` zur
JVM und `<base>.out` zurück. Mit der JVM redet der Agent nie direkt.
[docs/PROTOCOL.md](docs/PROTOCOL.md) beschreibt die Frames.

Das JavaScript des Agenten steckt in `protocol.exe`. Der Host baut das
eingeschleuste Skript im Speicher zusammen und schreibt nichts in den
Spielordner oder sonst wohin. Die Hooks leben nur im Speicher des Spiels und
verschwinden mit ihm.

## Erste Schritte

Normalerweise startet der Launcher den Host. Du kannst ihn aber auch von Hand
aus dem Installationsordner starten:

```
<Sacred Gold>\launcher\protocol.exe
```

Seine Pfade findet er neben der EXE, Argumente brauchst du also keine. Läuft
das Spiel als Administrator, starte den Host ebenfalls so. Sonst kann Frida
sich nicht einklinken, und der Host wartet weiter, obwohl er den Prozess sieht.

Java sucht der Host in dieser Reihenfolge: `java/bin/java.exe` neben sich
(dorthin entpackt der Launcher ein geladenes JDK), `%JAVA_HOME%\bin\java.exe`,
dann `java` aus dem `PATH`. Meist übergibt der Launcher sein Java mit `--java`.
Für einen echten Lauf brauchst du JDK 21 oder neuer.

| Flag | Was es tut |
|---|---|
| `--enable <ids>` | Lädt die Mods mit diesen kommagetrennten IDs |
| `--java <Pfad>` | Nimmt diese Java-EXE |
| `--dist <Pfad>` | Liest die JARs aus diesem Ordner |
| `--mods <Pfad>` | Liest die Mods aus diesem Ordner |
| `--agent <Pfad>` | Liest den Agenten aus einem Ordner statt aus der EXE, praktisch beim Bearbeiten |
| `--hooks` | Gibt die Hook-Stellen als JSON aus und beendet sich |

Bei einem unbekannten Argument bricht der Host mit einem Fehler ab.

## Frames

So sieht ein Ausschnitt einer Sitzung aus:

```
EVT 2 hero.captured class=9 className=Daemon level=142 hp=27127 maxHp=27127
ASK 3 health.damage entity=player damage=553 current=19849 next=19296 max=26999
END 3 set.next=19849
```

`EVT` meldet ein Ereignis und erwartet keine Antwort. `ASK` kommt von einem
Hook vor einem Schreibzugriff, und der Spiel-Thread steht, bis eine Antwort
da ist. `END` ist diese Antwort: Sie erlaubt den Schreibzugriff, bricht ihn ab
oder ersetzt Felder. Hier behält `set.next=19849` die alte Gesundheit, der
Schaden kommt also nie an.

Ein Mod kann auch selbst einen Befehl schicken:

```
CMD 1 player.gold
RES 1 ok=1 gold=104233
```

`BYE` beendet die Sitzung. Schlüssel und Werte kodieren nur die Zeichen mit
Prozent, die einen Frame kaputt machen würden: `%`, Leerzeichen, `=`, LF und
CR. Alles andere bleibt, wie es ist. So kannst du eine laufende Sitzung direkt
im Terminal lesen.

### Regeln für ein `ASK`

Ein `ASK` hält den Spiel-Thread fest, deshalb gelten drei Regeln:

- **Eine Antwort hat eine Frist.** Der Watchdog prüft alle 125 ms und stuft ein
  `ASK` als überfällig ein, sobald es älter als 250 ms ist. Dann antwortet der
  Host selbst mit `END ok=1` und protokolliert die Sequenznummer. Das passiert
  meist 250–375 ms nach der Anfrage, dazu kommen Verzögerungen von Scheduler
  und Sendeschleife. Der langsame Mod verliert für diese Anfrage sein Veto.
  Beendet sich die JVM, wartet der Host auf keine Antworten mehr und fährt
  herunter.
- **Lies keine Antworten im Thread, der antwortet.** Ruft ein Mod aus einem
  Event-Handler ins Spiel zurück, blockiert er es sonst. Beide Seiten lesen
  und verteilen Frames deshalb in getrennten Threads.
- **Frames haben ihren eigenen Kanal.** Ein Mod, der mit `System.out` schreibt,
  macht nichts kaputt: Seine Ausgabe landet in der Konsole des Hosts. Eine
  Zeile, die der Host nicht lesen kann, meldet er als unlesbare Ausgabe von
  Coderpack. Fürs Logging nimm die API des Loaders, sie stellt jeder Zeile die
  Mod-ID voran.

### Wenn der Host sich beendet

Unter Windows versucht der Host, die JVM in ein Job-Objekt mit
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` zu stecken, damit sie mit ihm endet. Das
Spiel kommt nie in diesen Job. Die Garantie gilt nur, wenn Anlegen, Limit und
Zuweisung der JVM alle klappen. Bei den ersten beiden Fehlern warnt der Host.
Ein Fehler bei der Zuweisung bleibt bisher stumm.

## Wenn das Spiel abstürzt

Mit diesen Flags findest du einen kaputten Hook ohne neuen Build:

| Flag | Was es tut |
|---|---|
| `--skip gold,position` | Lässt diese Agent-Module weg |
| `--only health` | Lädt nur dieses Modul. `core`, `bus` und `names` laden immer |
| `--no-hook goldEpilogue` | Behält das Modul, überspringt aber diese eine Stelle |
| `--trace` | Protokolliert jeden Hook, sobald er auslöst |
| `--no-ask` | Behält die Hooks, wartet aber nie auf eine Antwort |

Ein Modulname ist der Dateiname des Agenten ohne Nummernpräfix. Starte das
Spiel zwischen zwei Versuchen neu. Der eingeschleuste Agent kann nach dem Ende
des Hosts im Spiel bleiben, und ein neuer Host würde dann eine schon gehookte
Instruktion noch einmal patchen.

Die Liste der schaltbaren Stellen holt sich der Launcher über `--hooks`. Er
fragt die `protocol.exe` im Spielordner, denn nur diese Kopie weiß, welcher
Agent in ihr steckt.

## Bauen

Du brauchst Rust 1.98 mit der MSVC-Toolchain und LLVM: `frida-sys` startet
bindgen, und bindgen braucht libclang. `.cargo/config.toml` setzt
`LIBCLANG_PATH` auf `C:\Program Files\LLVM\bin`, falls die Variable noch nicht
gesetzt ist.

```
cargo build --release
cargo test --release
```

Das Ergebnis ist `target/release/protocol.exe`. Die Linker-Warnung `LNK4098`
entsteht, weil frida-core die CRT statisch und Rust sie dynamisch einbindet.
Sie ist erwartet.

Den Agenten nimmt der Build aus der ersten Quelle, die er findet:

1. `$PROTOCOL_AGENT`
2. Das benachbarte `../coderpack/agent/src`
3. Das `agent.zip` des coderpack-Releases, das in `dependencies.json`
   festgelegt ist, zwischengespeichert unter `build/agent/`

Dank der dritten Quelle baut auch ein einzelner Klon. coderpack erzeugt seine
Adresstabelle, statt sie einzuchecken. Ein Checkout reicht also nicht immer,
ein Release-Asset dagegen schon. Die Quelle seines Agenten gibt der Host beim
Start aus.

Die Tests prüfen den Frame-Codec, den Minifier, den Agent-Bundler und die
Grenze zur JVM. Zwei End-to-End-Tests starten eine echte Coderpack-JVM ohne
Mods und prüfen, dass jedes `ASK` einen Frame bekommt, den der Host lesen kann.
Sie nehmen die zuletzt gebauten JARs `api` und `zygote` aus einem benachbarten
coderpack-Checkout oder aus `~/.m2/repository/dev/ancaria/coderpack`. Fehlen
sie, melden die Tests einen Skip und bestehen. Ein reiner Rust-Checkout kommt
also ohne JDK aus.

`vendor/frida` ist eine gepatchte Kopie des Crates `frida` 0.17.2. Upstream
castet das `user_data` eines Callbacks bei allen Nachrichten außer `frida:rpc`
auf den falschen Typ, und schon das erste `send()` des Agenten ließ den Host
mit `0xC0000005` abstürzen. `vendor/README.md` beschreibt den Patch. Diese
Prüfung braucht `python` im `PATH`, aber nicht das Spiel:

```
cargo run --example message_check
```

## Releases

Die CI läuft unter Windows bei jedem Push auf `master`, bei jedem Pull Request
und auf Abruf. Sie baut und testet im Release-Modus und hängt `protocol.exe`
als Workflow-Artefakt an. `cargo fmt --check` läuft mit, lässt den Build aber
nicht scheitern.

Auf `master` liest die CI `version` aus `Cargo.toml`. Gibt es den Tag
`v<Version>` noch nicht, legt die CI ihn an und veröffentlicht ein Release mit
`protocol.exe`. Für ein Release hebst du die Version mit
`pwsh tools/version.ps1 <Version>` an. Der Launcher lädt diese Datei herunter,
wenn neben ihm kein protocol-Checkout liegt.

Eine Änderung am Agenten erreicht Spieler nur über ein Release dieses
Repositorys. Die Release-Reihenfolge für den ganzen Loader steht in
[CONTRIBUTING](https://github.com/ancaria-dev/.github/blob/master/CONTRIBUTING.DE.md).

## Lizenz

MIT, siehe [LICENSE](LICENSE).
