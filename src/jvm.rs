//! The mod JVM: a child process the host reaches over a pipe of its own, and
//! nothing native is loaded into it.  That pipe is the whole boundary, which is
//! why a crashing mod cannot crash the game.
//!
//! Normally the pipe is a named one, created here before the JVM starts and
//! passed to it as `--pipe`.  The JVM's own stdin, stdout and stderr are then
//! nobody's business but the mods', which is the point: see `pipe.rs`.  If the
//! pipe cannot be created the host says so and talks over stdin and stdout
//! instead, the way it always did, because a loud fallback beats no game.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::thread;

use anyhow::{Context, Result};

use crate::codec::Frame;
use crate::job::Job;
use crate::pipe::Listener;

pub struct Jvm {
    child: Child,
    lines: Sender<String>,
}

impl Jvm {
    /// Spawns Coderpack.  `on_frame` runs on the reader thread for every frame the
    /// JVM sends back, and it must not block for long.
    pub fn spawn<F>(
        java: &Path,
        classpath: &str,
        mods: &Path,
        enable: Option<&str>,
        job: Option<&Job>,
        on_frame: F,
    ) -> Result<Jvm>
    where
        F: Fn(Frame) + Send + 'static,
    {
        let listener = match Listener::bind() {
            Ok(listener) => Some(listener),
            Err(why) => {
                eprintln!(
                    "[host] Couldn’t open a private channel to Coderpack ({why}). \
                     Falling back to stdout, where a mod that prints can cost a frame"
                );
                None
            }
        };

        let mut command = Command::new(java);
        command
            .arg("-cp")
            .arg(classpath)
            .arg("dev.ancaria.coderpack.zygote.Main")
            .arg("--mods")
            .arg(mods);
        if let Some(ids) = enable {
            command.arg("--enable").arg(ids);
        }
        match &listener {
            // stdout is the JVM's again: whatever a mod prints goes straight to
            // this console, and nothing it prints can reach the protocol.
            Some(listener) => {
                command.arg("--pipe").arg(listener.name());
                command.stdin(Stdio::null()).stdout(Stdio::inherit());
            }
            None => {
                command.stdin(Stdio::piped()).stdout(Stdio::piped());
            }
        }
        let mut child = command
            .stderr(Stdio::inherit())
            .spawn()
            // The one failure a player actually hits: no JDK on the machine at
            // all.  Naming the path and the way out beats "cannot start java",
            // which reads as a bug in the host.
            .with_context(|| format!(
                "Could not start the JVM at {}—the mod loader requires JDK 21 or newer. \
                 The launcher can download one to <Sacred Gold>/launcher/java. \
                 To use a different installation, pass --java <path to java.exe>",
                java.display()))?;

        if let Some(job) = job {
            job.adopt(&child);
        }

        let (read, write): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match listener {
            Some(listener) => {
                let (read, write) = listener
                    .accept(&mut || matches!(child.try_wait(), Ok(None)))
                    .inspect_err(|_| {
                        let _ = child.kill();
                    })?;
                (Box::new(read), Box::new(write))
            }
            None => (
                Box::new(child.stdout.take().expect("piped")),
                Box::new(child.stdin.take().expect("piped")),
            ),
        };

        thread::spawn(move || {
            for line in BufReader::new(read).lines().map_while(Result::ok) {
                match Frame::decode(&line) {
                    Some(frame) => on_frame(frame),
                    None => eprintln!("[coderpack] Could not parse Coderpack output: {line}"),
                }
            }
            eprintln!("[coderpack] Coderpack closed its output stream");
        });

        let (lines, rx) = channel::<String>();
        thread::spawn(move || {
            // Reused rather than formatted afresh: a frame leaves as one write
            // because it was assembled as one, and this is the hot path in a
            // busy fight.
            let mut out = write;
            let mut buffer = Vec::with_capacity(256);
            for line in rx {
                buffer.clear();
                buffer.extend_from_slice(line.as_bytes());
                buffer.push(b'\n');
                if out.write_all(&buffer).is_err() {
                    break;
                }
            }
        });

        Ok(Jvm { child, lines })
    }

    pub fn send(&self, frame: &Frame) {
        let _ = self.lines.send(frame.encode());
    }

    pub fn sender(&self) -> Sender<String> {
        self.lines.clone()
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}
