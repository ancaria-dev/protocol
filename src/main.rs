//! protocol — injects the Coderpack agent into Sacred Gold and runs the mod JVM.
//!
//! The host is the only part that is allowed to be paranoid: the agent trusts
//! it, and it trusts nothing.  Concretely that means Coderpack never gets to block
//! the game (verdicts have a deadline), a dead Coderpack degrades to observation
//! instead of hanging, and the JVM cannot outlive the host.

mod agent;
mod codec;
mod job;
mod jvm;
mod router;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use frida::{Device, DeviceManager, Frida, Script, ScriptOption};
use serde_json::json;

use codec::Frame;
use job::Job;
use jvm::Jvm;
use router::{from_sal, Pending, Router, ToAgent};

/// How long Coderpack may take to answer an ASK before the host answers for it.
/// The game thread is stopped for exactly this long in the worst case.
const VERDICT_DEADLINE: Duration = Duration::from_millis(250);

/// The game executable, in the order it is looked for. Most installs are the
/// community HD wrapper, but the stock game is `Sacred.exe` and some copies
/// were renamed, so the host takes the first of these that is running.
/// Windows does not care about the case of a file name, so neither does the
/// match.
const GAME_PROCESSES: [&str; 3] = ["pureHD.exe", "Sacred.exe", "Game.exe"];

struct Config {
    agent: PathBuf,
    classpath: String,
    mods: PathBuf,
    java: PathBuf,
    skip: Vec<String>,
    only: Vec<String>,
    ask: bool,
    no_hook: Vec<String>,
    trace: bool,
    /// Mod ids the launcher ticked. None means every jar in the folder.
    enable: Option<String>,
}

impl Config {
    fn from_args() -> Result<Config> {
        let mut agent = default_agent();
        let mut dist = default_dist();
        let mut mods: Option<PathBuf> = None;
        let mut java = java_path();
        let mut skip = Vec::new();
        let mut only = Vec::new();
        let mut ask = true;
        let mut no_hook = Vec::new();
        let mut trace = false;
        let mut enable = None;

        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            let value = args.get(i + 1).cloned().unwrap_or_default();
            match args[i].as_str() {
                "--agent" => agent = PathBuf::from(value),
                "--dist" => dist = PathBuf::from(value),
                "--mods" => mods = Some(PathBuf::from(value)),
                "--enable" => enable = Some(value),
                "--java" => java = PathBuf::from(value),
                "--skip" => skip = value.split(',').map(str::to_string).collect(),
                "--only" => only = value.split(',').map(str::to_string).collect(),
                "--no-hook" => {
                    no_hook = value.split(',').map(str::to_string).collect();
                }
                "--trace" => {
                    trace = true;
                    i -= 1;   // a flag, not a pair
                }
                "--no-ask" => {
                    ask = false;
                    i -= 1;   // a flag, not a pair
                }
                other => return Err(anyhow!("Unknown argument: {other}")),
            }
            i += 2;
        }

        // Paths are relative to the executable unless they resolve from the
        // working directory, so dropping the installed folder into the game and
        // double clicking the exe works without arguments.
        let agent = beside_exe(agent);
        let dist = beside_exe(dist);

        let classpath = format!(
            "{};{}",
            dist.join("api.jar").display(),
            dist.join("zygote.jar").display()
        );
        Ok(Config { agent, classpath,
                    mods: mods.unwrap_or_else(|| default_mods(&dist)), java, skip, only, ask, no_hook, trace, enable })
    }
}

/// Where the bundled agent lives: next to the executable when installed, or in
/// the working tree when run from a checkout.  Identified by the generated
/// address table, which is the one file that is always there.
fn default_agent() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let candidates = [
        exe_dir.join("agent"),
        // Running from a checkout: the agent is the coderpack repository's,
        // next door.
        PathBuf::from("../coderpack/agent/src"),
        PathBuf::from("agent/src"),
    ];
    candidates
        .iter()
        .find(|dir| dir.join("gen/addr.js").is_file())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("agent"))
}

/// Where the jars live.  "." always exists, so it cannot be a default.  The
/// directory is identified by the loader jar being in it.
fn default_dist() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    for candidate in [exe_dir, PathBuf::from("dist"), PathBuf::from(".")] {
        if candidate.join("zygote.jar").is_file() {
            return candidate;
        }
    }
    PathBuf::from("dist")
}

/// Mods belong to the game, not to Coderpack: with the loader installed in
/// `<Sacred Gold>/launcher`, they live one level up in `<Sacred Gold>/mods`.
fn default_mods(dist: &Path) -> PathBuf {
    let beside_game = dist.join("../mods");
    if beside_game.is_dir() {
        return beside_game;
    }
    dist.join("mods")
}

fn beside_exe(path: PathBuf) -> PathBuf {
    if path.exists() || path.is_absolute() {
        return path;
    }
    match std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        Some(dir) => dir.join(path),
        None => path,
    }
}

/// Which `java` starts Coderpack when `--java` did not say.
///
/// `<install>/java/bin/java.exe` comes first: that is where the launcher puts a
/// JDK it downloaded for a player who had none, and it is the one it passes
/// with `--java`.  A host started by hand out of the same folder has to reach
/// the same JVM, or debugging a mod means debugging it on a different Java than
/// the one it ran on a minute ago.
fn java_path() -> PathBuf {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|dir| dir.join("java").join("bin").join("java.exe"));
    if let Some(path) = beside {
        if path.is_file() {
            return path;
        }
    }
    match std::env::var("JAVA_HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home).join("bin/java.exe"),
        _ => PathBuf::from("java"),
    }
}

/// The one version a player sees.  Written beside the executable by the build;
/// missing only when the host is run straight out of a checkout.
fn version() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("VERSION")))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|| "dev".to_string())
}

fn main() -> Result<()> {
    let config = Config::from_args()?;
    println!("[host] Sacred Mod Loader {}", version());
    let source = agent::bundle(&config.agent, &config.skip, &config.only, config.ask, &config.no_hook, config.trace)?;
    println!("[host] Agent bundled: {} bytes{}", source.len(),
             if config.ask { "" } else { " (verdicts disabled)" });

    let pending = Pending::default();
    let (work_tx, work_rx) = channel::<ToAgent>();

    // The job object is what guarantees the JVM cannot outlive us.
    let job = Job::create();
    if job.is_none() {
        eprintln!("[host] Couldn’t create a job object. The JVM may keep running if the host crashes");
    }

    let mut jvm = {
        let tx = work_tx.clone();
        let pending = pending.clone();
        Jvm::spawn(&config.java, &config.classpath, &config.mods,
                   config.enable.as_deref(), job.as_ref(),
                   move |frame| {
                       if let Some(work) = from_sal(&frame, &pending) {
                           let _ = tx.send(work);
                       }
                   })?
    };
    println!("[host] Coderpack started");

    spawn_watchdog(pending.clone(), work_tx.clone());

    let frida = unsafe { Frida::obtain() };
    let manager = DeviceManager::obtain(&frida);
    let device = manager
        .get_local_device()
        .context("No local Frida device is available")?;

    loop {
        if !jvm.alive() {
            return Err(anyhow!("Coderpack exited. Nothing left to run"));
        }
        let (pid, name) = wait_for_game(&device);
        println!("[host] Attaching to {name}, PID {pid}");

        let session = device.attach(pid).context(
            "Couldn’t attach to the game. Run this terminal as Administrator \
             if the game is running with elevated privileges",
        )?;
        let mut options = ScriptOption::new().set_name("sal");
        let mut script = session.create_script(&source, &mut options)?;
        script.handle_message(Router {
            to_sal: jvm.sender(),
            pending: pending.clone(),
            dropped: Arc::new(Mutex::new(0)),
        })?;
        script.load()?;
        println!("[host] Agent loaded");

        pump(&script, &work_rx, &mut jvm, || session.is_detached());

        // Drop the script explicitly and give frida a moment to finish any
        // callback already in flight: tearing it down implicitly while the
        // target is dying faulted the host itself on the first real run.
        let _ = script.unload();
        drop(script);
        thread::sleep(Duration::from_millis(250));

        println!("[host] Detached. Waiting for the game to start again");
        jvm.send(&Frame::new("EVT", 0, "session.detached"));
    }
}

/// Owns the Script and is the only thread that posts to it.
fn pump<F: Fn() -> bool>(
    script: &Script,
    work: &Receiver<ToAgent>,
    jvm: &mut Jvm,
    detached: F,
) {
    loop {
        match work.recv_timeout(Duration::from_millis(200)) {
            Ok(item) => post(script, item),
            Err(RecvTimeoutError::Timeout) => {
                if detached() {
                    return;
                }
                if !jvm.alive() {
                    // Rule: a dead Coderpack must not hang the game.  Stop asking;
                    // the agent keeps observing.
                    eprintln!("[host] Coderpack stopped. Continuing in observe-only mode");
                    post(script, ToAgent::Asking(false));
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Fields go to the agent as strings: the wire format has no types, and the
/// agent parses what it knows the shape of.
fn strings(fields: Vec<(String, String)>) -> serde_json::Map<String, serde_json::Value> {
    fields
        .into_iter()
        .map(|(key, value)| (key, serde_json::Value::String(value)))
        .collect()
}

fn post(script: &Script, item: ToAgent) {
    let message = match item {
        ToAgent::Verdict { seq, cancel, set } => json!({
            "type": "verdict",
            "seq": seq,
            "cancel": cancel,
            "set": strings(set),
        }),
        ToAgent::Command { seq, name, fields } => json!({
            "type": "cmd",
            "seq": seq,
            "name": name,
            "f": strings(fields),
        }),
        ToAgent::Asking(enabled) => json!({ "type": "mode", "ask": enabled }),
    };
    if let Err(err) = script.post(message.to_string(), None) {
        eprintln!("[host] Could not send a message to the agent: {err}");
    }
}

/// Answers on Coderpack's behalf when a mod is too slow.  A hung mod must never hang
/// the game, so it loses its veto instead.
fn spawn_watchdog(pending: Pending, work: std::sync::mpsc::Sender<ToAgent>) {
    thread::spawn(move || loop {
        thread::sleep(VERDICT_DEADLINE / 2);
        for seq in pending.overdue(VERDICT_DEADLINE) {
            eprintln!("[host] Coderpack did not answer request {seq} in time. Continuing");
            pending.close(seq);
            let _ = work.send(ToAgent::Verdict {
                seq,
                cancel: false,
                set: Vec::new(),
            });
        }
    });
}

/// Blocks until one of `GAME_PROCESSES` is running, and says which it was.
/// The list is in preference order, so a folder holding two of them attaches
/// to the one the loader was built against.
fn wait_for_game(device: &Device) -> (u32, String) {
    loop {
        let running = device.enumerate_processes();
        for wanted in GAME_PROCESSES {
            let found = running
                .iter()
                .find(|p| p.get_name().eq_ignore_ascii_case(wanted));
            if let Some(process) = found {
                return (process.get_pid(), process.get_name().to_string());
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
}
