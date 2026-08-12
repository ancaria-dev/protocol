<div align="center">

[![Rust](https://img.shields.io/badge/Rust-1.98-CE422B?style=for-the-badge&logo=rust&logoColor=white&labelColor=1C1410)](https://www.rust-lang.org)
[![Frida](https://img.shields.io/badge/Frida-0.17.2-3B5998?style=for-the-badge&labelColor=1C1410)](https://frida.re)
[![License](https://img.shields.io/badge/License-MIT-4B5563?style=for-the-badge&labelColor=1C1410)](LICENSE)
[![Sacred Community](https://img.shields.io/badge/Sacred-Community-8B1A1A?style=for-the-badge&labelColor=1C1410)](https://ancaria.dev)

</div>

[Русский](README.md) · [English](README.EN.md)

# Sacred Communication Protocol

Sacred Gold basiert auf einem 32-Bit-Spiel aus dem Jahr 2004. Die vom Mod-Loader
verwendete 64-Bit-JVM kann deshalb nicht im Spielprozess laufen. Dieses
Repository enthält das zeilenbasierte Protokoll zwischen beiden Seiten und den
Windows-Host, der die Nachrichten überträgt.

Die Mods laufen in einer eigenen `java.exe`. Im Spiel arbeitet ein von Frida
eingeschleuster JavaScript-Agent. Er sendet Frida-Nachrichten an den Host, der
sie in Protokoll-Frames umwandelt. Zwischen Host und JVM laufen diese Frames
zeilenweise über `stdout` und `stdin`. Das genaue Format beschreibt
[docs/PROTOCOL.md](docs/PROTOCOL.md). Der Host schreibt in stdin der JVM und
liest deren stdout. Der Agent spricht nie direkt mit der JVM.

Die Programmdatei wartet auf den Spielprozess, lädt den Agenten aus dem
Repository [coderpack](https://github.com/ancaria-dev/coderpack), startet
`dev.ancaria.coderpack.zygote.Main` mit `api.jar` und `zygote.jar` im
Klassenpfad und vermittelt anschließend in beide Richtungen. Dabei wird keine
Spieldatei verändert. Die Hooks liegen nur im Arbeitsspeicher und verschwinden
mit dem Spielprozess.

## Starten

Im Normalbetrieb startet der Launcher den Host. Aus dem installierten
Verzeichnis lässt er sich auch direkt aufrufen:

```
<Sacred Gold>\launcher\protocol.exe
```

Läuft das Spiel mit Administratorrechten, braucht auch der Host erhöhte Rechte.
Andernfalls kann Frida trotz sichtbarem Prozess nicht anhängen und der Host
wartet unbegrenzt. Ohne Argumente sucht er Agent, JAR-Dateien und Java-Laufzeit
neben `protocol.exe` beziehungsweise in den dafür vorgesehenen
Standardverzeichnissen.

## Format der Verbindung

Drei Frames aus einer Sitzung:

```
EVT 2 hero.captured class=9 className=Daemon level=142 hp=27127 maxHp=27127
ASK 3 health.damage entity=player damage=553 current=19849 next=19296 max=26999
END 3 set.next=19849
```

`EVT` meldet ein eingetretenes Ereignis und erwartet keine Antwort. `ASK` stammt
von einem Hook vor dem Schreibzugriff. Bis zum Verdikt hält dieser Hook den
Spiel-Thread an. Mit `END` lässt ein Mod den Vorgang unverändert passieren,
bricht ihn ab oder schreibt Felder um. Im Beispiel setzt `set.next` den
Lebenspunktewert zurück, bevor das Spiel ihn festschreibt. Wenn ein Mod selbst
eine Aktion anstößt, laufen Befehle in die Gegenrichtung:

```
CMD 1 player.gold
RES 1 ok=1 gold=104233
```

Hinzu kommen `LOG` und `BYE`. Bei Schlüsseln und Werten werden nur Zeichen
prozentkodiert, die das Zeilenformat beschädigen würden: Leerzeichen als `%20`,
`=` als `%3D`, LF als `%0A`, CR als `%0D` und `%` als `%25`. Alle anderen
Zeichen bleiben unverändert. Eine laufende Sitzung bleibt dadurch im Terminal
lesbar.

Weil `ASK` den Spiel-Thread blockiert, gelten drei Regeln:

- **Ein Verdikt hat eine Frist.** Der Watchdog prüft alle 125 ms und betrachtet
  ein `ASK` erst als überfällig, wenn es älter als 250 ms ist. Dann stellt der
  Host `END ok=1` in die Warteschlange und protokolliert die Sequenznummer. Das
  geschieht gewöhnlich 250 bis 375 ms nach der Anfrage, zuzüglich der
  Verzögerung durch Scheduler und Sendeschleife. Der Mod verliert sein Veto für
  diese Anfrage. Wenn die JVM endet, schaltet der Host weitere `ASK`-Wartezeiten
  ab und beendet sich anschließend.
- **Wer ein `ASK` beantwortet, darf im selben Thread keine Antworten lesen.** Ein
  Mod, der aus einem Event-Handler heraus wieder das Spiel aufruft, verursacht
  sonst einen Deadlock. Empfang und Versand laufen deshalb getrennt.
- **Über `stdout` dürfen nur Frames laufen.** Diagnoseausgaben gehören nach
  `stderr`. Schreibt ein Mod mit `System.out`, kann der Host die betreffende
  Zeile nicht als Frame lesen und meldet sie als nicht interpretierbare
  Coderpack-Ausgabe. Für Logs stellt der Loader eine eigene Schnittstelle bereit.

Unter Windows versucht der Host, ausschließlich die JVM in ein Windows-Jobobjekt
mit `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` aufzunehmen. Gelingt die Zuordnung,
beendet Windows die JVM zusammen mit dem Host. Schlägt das Anlegen oder
Konfigurieren des Jobobjekts fehl, läuft der Host mit einer Warnung weiter.
Ein Fehler bei der Prozesszuordnung bleibt derzeit ohne Meldung. Die
Kill-on-close-Garantie gilt deshalb nur nach erfolgreicher Erstellung,
Konfiguration und Zuordnung. Der Spielprozess gehört nie zu diesem Jobobjekt.

## Wenn das Spiel abstürzt

Bei Hooks in einer alten Binärdatei kann schon eine einzelne Stelle den
Spielprozess zum Absturz bringen. Mit den folgenden Optionen lässt sich der
Auslöser zwischen zwei Versuchen ohne erneuten Build eingrenzen. Nach jedem
Versuch muss das Spiel neu gestartet werden. Wenn nur der Host endet, wird ein
injizierter Agent nicht zuverlässig entladen. Ein neuer Host kann dann
versuchen, eine bereits gehookte Instruktion erneut zu verändern.

| Flag | Wirkung |
|---|---|
| `--skip gold,position` | die Agent-Module `gold` und `position` aus dem Bundle ausschließen |
| `--only health` | nur `health` laden, wobei `core`, `bus` und `names` immer enthalten sind |
| `--no-hook goldEpilogue` | das Modul behalten, aber diese Hook-Stelle nicht anbinden |
| `--trace` | jeden Hook melden, sobald er ausgelöst wird |
| `--no-ask` | Hooks installieren, aber Verdikte abschalten |

Modulnamen sind die Dateinamen des Agenten ohne Zahlenpräfix. `--agent`,
`--dist`, `--mods` und `--java` überschreiben die ermittelten Pfade. Mit
`--enable` lässt sich festlegen, welche Mod-IDs geladen werden. Ohne `--java`
verwendet der Host zuerst `java/bin/java.exe` neben der eigenen Programmdatei.
Dort legt der Launcher ein heruntergeladenes JDK ab. Danach prüft er
`%JAVA_HOME%\bin\java.exe` und zuletzt `java` im `PATH`. Der Launcher übergibt
seine heruntergeladene Java-Laufzeit normalerweise ausdrücklich mit `--java`.
Ein echter Host-Lauf benötigt JDK 21 oder neuer. Ein unbekanntes Argument
beendet den Start mit einem Fehler.

## Bauen

```
cargo build --release
```

Das Ergebnis liegt unter `target/release/protocol.exe`. Für den Build werden
Rust 1.98 mit der MSVC-Toolchain und eine LLVM-Installation benötigt.
`frida-sys` führt `bindgen` aus, das wiederum `libclang` braucht. In
`.cargo/config.toml` ist `LIBCLANG_PATH` auf
`C:\Program Files\LLVM\bin` gesetzt. Eine bereits vorhandene Umgebungsvariable
hat Vorrang. Die Linkerwarnung `LNK4098` entsteht durch die statische CRT von
frida-core und die dynamische CRT von Rust. Sie ist in dieser Konfiguration
erwartet.

`vendor/frida` enthält eine gepatchte Kopie des Crates `frida` 0.17.2. Bei jeder
Nachricht außer `frida:rpc` interpretiert der Upstream-Code den Zeiger
`user_data` fälschlich als Handler-Typ des Aufrufers. Tatsächlich zeigt er auf
einen `CallbackHandler`. Daher stürzte der Host bereits beim ersten `send()` des
Agenten mit `0xC0000005` ab, bevor eine Nachricht ankam. Einzelheiten stehen in
`vendor/README.md`. Der folgende Regressionstest benötigt `python` im `PATH`,
hängt sich an einen kurzlebigen Prozess und wartet bis zu 5 Sekunden auf eine
Nachricht. Ein laufendes Spiel ist nicht nötig:

```
cargo run --example message_check
```

`cargo test --release` führt derzeit sechs Tests für Frame-Codec, Agent-Bundler
und JVM-Grenze aus. Die beiden Bundle-Tests verwenden den Fixture-Agenten aus
`tests/agent` und greifen nie auf `../coderpack` zu. Der End-to-End-Test startet
eine echte Coderpack-JVM ohne Mods und prüft, ob jedes `ASK` mit einem lesbaren
Frame beantwortet wird. Er wählt die lexikalisch neuesten `api`- und
`zygote`-JAR-Dateien unter `../coderpack/*/build/libs` oder im lokalen
Maven-Repository unter `~/.m2/repository/dev/ancaria/coderpack/`. Findet er
keine, gibt er eine Meldung aus, überspringt den Lauf und gilt als bestanden.
Ein reiner Rust-Checkout benötigt daher kein JDK.

CI läuft unter Windows bei Pushes auf `master`, für Pull Requests und bei
manuellem Start. `cargo fmt --check` blockiert den Build nicht. Danach folgen
Release-Build und Release-Tests. Jeder Lauf lädt `protocol.exe` als
Workflow-Artefakt hoch. Nach einem erfolgreichen Push auf `master` liest CI
`version` aus `Cargo.toml`. Existiert auf dem Remote noch kein Tag
`v<version>`, erstellt der Veröffentlichungsschritt diesen Tag und ein Release
mit `target/release/protocol.exe`. Eine Versionsänderung wird erst mit einem
erfolgreichen Veröffentlichungsschritt ausgeliefert. Dieses Asset lädt der
Launcher, wenn beim Build kein benachbartes `protocol`-Checkout vorhanden ist.

## Lizenz

Der Code steht unter der MIT-Lizenz. Der vollständige Lizenztext befindet sich
in [LICENSE](LICENSE).
