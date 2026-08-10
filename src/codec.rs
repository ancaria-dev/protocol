//! Frames on the wire to Coderpack.  See docs/PROTOCOL.md.
//!
//! Flat key=value lines rather than JSON: the JDK ships no JSON parser, and a
//! line stays readable in a terminal, which is most of what this layer does.

use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub struct Frame {
    pub verb: String,
    pub seq: u64,
    pub name: String,
    pub fields: Vec<(String, String)>,
}

impl Frame {
    pub fn new(verb: &str, seq: u64, name: &str) -> Self {
        Frame {
            verb: verb.to_string(),
            seq,
            name: name.to_string(),
            fields: Vec::new(),
        }
    }

    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Fields whose key starts with `set.`, i.e. a verdict's rewrites.
    pub fn rewrites(&self) -> Vec<(String, String)> {
        self.fields
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix("set.")
                    .map(|name| (name.to_string(), v.clone()))
            })
            .collect()
    }

    pub fn encode(&self) -> String {
        let mut line = String::new();
        let _ = write!(line, "{} {} {}", self.verb, self.seq, self.name);
        for (key, value) in &self.fields {
            let _ = write!(line, " {}={}", encode(key), encode(value));
        }
        line
    }

    pub fn decode(line: &str) -> Option<Frame> {
        let mut parts = line.split(' ').filter(|p| !p.is_empty());
        let verb = parts.next()?.to_string();
        if verb == "BYE" {
            return Some(Frame::new("BYE", 0, ""));
        }
        let seq = parts.next()?.parse().ok()?;

        // A token carrying '=' is a field, never the name.  END frames have no
        // name, so treating the third token as one silently ate every verdict:
        // `END 3 cancel=1` parsed as a frame named "cancel=1" with no fields,
        // and nothing was ever cancelled or rewritten.
        let mut name = String::new();
        let mut fields = Vec::new();
        for part in parts {
            match part.split_once('=') {
                Some((key, value)) if !key.is_empty() => {
                    fields.push((decode(key), decode(value)));
                }
                _ if name.is_empty() => name = decode(part),
                _ => {}
            }
        }
        Some(Frame { verb, seq, name, fields })
    }
}

#[cfg(test)]
mod tests {
    use super::Frame;

    #[test]
    fn verdicts_keep_their_fields() {
        let end = Frame::decode("END 3 cancel=1").expect("parses");
        assert_eq!(end.name, "");
        assert_eq!(end.field("cancel"), Some("1"));

        let rewrite = Frame::decode("END 4 set.next=19849").expect("parses");
        assert_eq!(rewrite.rewrites(), vec![("next".into(), "19849".into())]);
    }

    #[test]
    fn named_frames_still_parse() {
        let cmd = Frame::decode("CMD 7 player.teleport x=1 y=2").expect("parses");
        assert_eq!(cmd.name, "player.teleport");
        assert_eq!(cmd.field("y"), Some("2"));
    }

    #[test]
    fn round_trips_through_encode() {
        let mut frame = Frame::new("EVT", 9, "item.pickup");
        frame.fields.push(("name".into(), "Lesser Healing Potion".into()));
        let back = Frame::decode(&frame.encode()).expect("parses");
        assert_eq!(back.name, "item.pickup");
        assert_eq!(back.field("name"), Some("Lesser Healing Potion"));
    }
}

/// Percent-encode only what would break the line format, so values stay
/// readable.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            '=' => out.push_str("%3D"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            _ => out.push(c),
        }
    }
    out
}

fn decode(value: &str) -> String {
    if !value.contains('%') {
        return value.to_string();
    }
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Runs the real Coderpack against the real codec, with no game involved.
///
/// `tests/replay.py` checks what Coderpack emits; this checks what the host makes of
/// it. That gap is not academic: verdicts were parsed with their fields eaten
/// for several builds while the Python test passed every time -- which is what
/// `verdicts_keep_their_fields` above pins down on literal lines. What only a
/// live Coderpack can show is that every ASK is answered at all, by whatever mods
/// happen to be installed: an unanswered one stops the game thread until the
/// host's deadline fires.
#[cfg(test)]
mod endtoend {
    use super::Frame;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    /// The zygote and its API, from wherever this machine happens to have
    /// them: a coderpack checkout beside this one, or the local Maven
    /// repository that `publishToMavenLocal` writes into. Neither is required
    /// -- a lone clone of this repository skips this test rather than failing,
    /// because what it covers is the boundary, and the other side of that
    /// boundary is another repository's build.
    fn jars() -> Option<(PathBuf, PathBuf)> {
        let newest = |dir: PathBuf, part: &str| -> Option<PathBuf> {
            std::fs::read_dir(dir).ok()?.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().unwrap_or_default()
                                .to_string_lossy().to_string();
                    name.starts_with(part) && name.ends_with(".jar")
                        && !name.contains("-sources") && !name.contains("-javadoc")
                })
                .max()
        };
        let sibling = |part: &str| {
            newest(PathBuf::from("../coderpack").join(part).join("build/libs"), part)
        };
        let m2 = |part: &str| {
            let base = home()?.join(".m2/repository/dev/ancaria/coderpack").join(part);
            std::fs::read_dir(base).ok()?.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .filter_map(|version| newest(version, part))
                .max()
        };
        let find = |part: &str| sibling(part).or_else(|| m2(part));
        Some((find("api")?, find("zygote")?))
    }

    fn home() -> Option<PathBuf> {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    }

    #[test]
    fn zygote_answers_every_ask() {
        let Some((api, zygote)) = jars() else {
            eprintln!("Skipped: No Coderpack JARs found in ../coderpack or ~/.m2");
            return;
        };
        let classpath = format!("{};{}", api.display(), zygote.display());
        // No mods on purpose.  What is under test is the boundary -- every ASK
        // comes back as a frame the host can read -- and a mod that asks the
        // game a question would have this test waiting on a host that is not
        // running.  Mod behaviour is tests/replay.py's job.
        let mut zygote = Command::new("java")
            .args(["-cp", &classpath, "dev.ancaria.coderpack.zygote.Main",
                   "--mods"])
            .arg("no-mods-here")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("java");

        let mut stdin = zygote.stdin.take().expect("piped");
        for line in [
            "EVT 0 hero.captured cls=1 clsName=Seraphim level=10 hp=100 maxHp=100 gold=0 exp=0",
            "ASK 1 health.damage kind=damage damage=30 prev=100 next=70 max=100",
            "ASK 2 gold.delta delta=100 current=0 dir=gain",
            "ASK 3 item.pickup ref=8814 name=TYPE_OBJECT_RING_FIRE01 level=30 player=1",
            "EVT 0 weather.rain_start intensity=3",
            "BYE",
        ] {
            writeln!(stdin, "{line}").expect("write");
        }
        drop(stdin);

        let mut replies = Vec::new();
        for line in BufReader::new(zygote.stdout.take().expect("piped"))
            .lines()
            .map_while(Result::ok)
        {
            replies.push(Frame::decode(&line).expect("host must parse Coderpack"));
        }
        let _ = zygote.wait();

        for seq in [1, 2, 3] {
            let verdict = replies
                .iter()
                .find(|f| f.seq == seq && f.verb == "END")
                .unwrap_or_else(|| panic!("no verdict for ask {seq}"));
            // No name, and the decision in the fields.  A verdict that parsed
            // as a *named* frame is how the fields used to get eaten.
            assert!(verdict.name.is_empty(), "END frames carry no name");
            assert!(verdict.field("ok").is_some()
                        || verdict.field("cancel").is_some()
                        || !verdict.rewrites().is_empty(),
                    "ask {seq} came back with no decision in it");
        }
    }
}
