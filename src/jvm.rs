//! The mod JVM: a child process whose stdin carries frames and whose stdout
//! carries verdicts and commands.  Nothing native is loaded into it -- the pipe
//! is the whole boundary, which is why a crashing mod cannot crash the game.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::thread;

use anyhow::{Context, Result};

use crate::codec::Frame;
use crate::job::Job;

pub struct Jvm {
    child: Child,
    lines: Sender<String>,
}

impl Jvm {
    /// Spawns Coderpack.  `on_frame` runs on the reader thread for every frame the
    /// JVM sends back; it must not block for long.
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
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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

        let stdout = child.stdout.take().expect("piped");
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match Frame::decode(&line) {
                    Some(frame) => on_frame(frame),
                    None => eprintln!("[coderpack] Could not parse Coderpack output: {line}"),
                }
            }
            eprintln!("[coderpack] Coderpack closed its output stream");
        });

        let mut stdin = child.stdin.take().expect("piped");
        let (lines, rx) = channel::<String>();
        thread::spawn(move || {
            for line in rx {
                if writeln!(stdin, "{line}").is_err() {
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
