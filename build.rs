//! Puts the Coderpack agent inside the executable.
//!
//! The host used to read the agent out of a folder beside it, which meant the
//! launcher had to unpack thirteen JavaScript files into the game folder and
//! keep them in step with the binary that loads them. Now the modules are
//! minified here and linked in, and the bundle is assembled in memory at
//! attach: nothing writes JavaScript to disk any more, and there is no second
//! copy to go stale. `--agent <path>` still reads a folder, which is what a
//! person editing the agent wants.
//!
//! Where the JavaScript comes from, in order:
//!
//!   1. `$PROTOCOL_AGENT`, which is what the launcher's build passes.
//!   2. The sibling `../coderpack/agent/src`.
//!   3. The `agent.zip` asset of the coderpack release pinned in
//!      `dependencies.json`, cached under `build/agent/`.
//!
//! The third is what makes a lone clone of this repository build: coderpack
//! generates its address table rather than committing it, so a checkout is not
//! always enough, and a release asset always is.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/js.rs"]
mod js;

fn main() {
    println!("cargo:rerun-if-env-changed=PROTOCOL_AGENT");
    println!("cargo:rerun-if-changed=dependencies.json");
    println!("cargo:rerun-if-changed=src/js.rs");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let (dir, origin) = locate(&manifest);
    embed(&dir, &origin, &out);
}

/// Finds a directory holding the agent, and says where it came from.
fn locate(manifest: &Path) -> (PathBuf, String) {
    if let Ok(path) = env::var("PROTOCOL_AGENT") {
        let dir = PathBuf::from(&path);
        if !dir.join("gen/addr.js").is_file() {
            panic!(
                "PROTOCOL_AGENT points at {path}, which has no gen/addr.js. \
                 Run `python tools/addr.py` in that coderpack checkout."
            );
        }
        return (dir, path);
    }

    let sibling = manifest.join("../coderpack/agent/src");
    if sibling.join("gen/addr.js").is_file() {
        return (sibling, "../coderpack/agent/src".to_string());
    }
    if sibling.is_dir() {
        // The table is generated, never committed, so a fresh coderpack
        // checkout has the modules and not the addresses. Falling back is
        // right (the release asset is a coherent set), but silently building
        // somebody's edited agent out of a release is not, so it is said out
        // loud.
        println!(
            "cargo:warning=../coderpack/agent/src has no gen/addr.js, so the pinned \
             coderpack release is being used instead. Run `python tools/addr.py` in \
             that checkout to embed your own agent."
        );
    }

    let version = pinned(manifest, "ancaria-dev/coderpack");
    let cache = manifest.join("build/agent").join(&version);
    if !cache.join("gen/addr.js").is_file() {
        fetch(&version, &cache);
    }
    (cache, format!("coderpack v{version}"))
}

/// The release this repository is pinned to for a cross-repository asset.
/// Same file shape as the launcher's, so one Renovate manager bumps both.
fn pinned(manifest: &Path, path: &str) -> String {
    let file = manifest.join("dependencies.json");
    let text = fs::read_to_string(&file)
        .unwrap_or_else(|err| panic!("Could not read {}: {err}", file.display()));
    let entries: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("Could not parse {}: {err}", file.display()));
    entries
        .as_array()
        .and_then(|list| {
            list.iter().find(|entry| entry["path"] == path).and_then(|entry| {
                entry["version"].as_str().map(str::to_string)
            })
        })
        .unwrap_or_else(|| panic!("No version is pinned for {path} in dependencies.json"))
}

/// Downloads and unpacks `agent.zip` from a public release. No token: needing
/// one to build the host would defeat the point of the download path.
fn fetch(version: &str, into: &Path) {
    let url = format!(
        "https://github.com/ancaria-dev/coderpack/releases/download/v{version}/agent.zip"
    );
    let holder = into.parent().unwrap_or(into).to_path_buf();
    let zip = holder.join(format!("agent-{version}.zip"));
    fs::create_dir_all(&holder).ok();
    let _ = fs::remove_dir_all(into);
    fs::create_dir_all(into).expect("could not create the agent cache");

    println!("cargo:warning=Downloading {url}");
    // curl ships with Windows 10 and later and with most Unix systems;
    // PowerShell is the fallback on a Windows without it.
    let target = zip.display().to_string();
    let downloaded = run(&[
        ("curl", vec!["-fsSL", &url, "-o", &target]),
        (
            "powershell",
            vec![
                "-NoProfile",
                "-Command",
                &format!("Invoke-WebRequest -Uri '{url}' -OutFile '{target}' -UseBasicParsing"),
            ],
        ),
    ]);
    if !downloaded {
        panic!(
            "Could not download {url}. Clone https://github.com/ancaria-dev/coderpack.git \
             beside this repository, point PROTOCOL_AGENT at its agent/src, or pin a \
             released version in dependencies.json."
        );
    }

    let folder = into.display().to_string();
    // bsdtar reads zip archives and is `tar` on Windows 10 and later and on
    // macOS; GNU tar is not, which is what unzip is here for.
    let unpacked = run(&[
        ("tar", vec!["-xf", &target, "-C", &folder]),
        ("unzip", vec!["-q", &target, "-d", &folder]),
        (
            "powershell",
            vec![
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -Path '{target}' -DestinationPath '{folder}' -Force"
                ),
            ],
        ),
    ]);
    let _ = fs::remove_file(&zip);
    if !unpacked || !into.join("gen/addr.js").is_file() {
        let _ = fs::remove_dir_all(into);
        panic!(
            "agent.zip from coderpack v{version} did not unpack into a folder with \
             gen/addr.js in it."
        );
    }
}

/// Runs the first of these commands that exists and succeeds.
fn run(attempts: &[(&str, Vec<&str>)]) -> bool {
    for (program, args) in attempts {
        match Command::new(program).args(args).status() {
            Ok(status) if status.success() => return true,
            _ => continue,
        }
    }
    false
}

/// Minifies every module into `OUT_DIR` and writes the table `agent.rs` links.
fn embed(dir: &Path, origin: &str, out: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let staged = out.join("agent");
    let _ = fs::remove_dir_all(&staged);
    fs::create_dir_all(&staged).expect("could not create the staging folder");

    let addr = dir.join("gen/addr.js");
    println!("cargo:rerun-if-changed={}", addr.display());
    fs::write(staged.join("addr.js"), js::compact(&read(&addr)))
        .expect("could not stage addr.js");

    let mut modules: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("Could not read {}: {err}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "js"))
        .collect();
    // Load order is the file-name order, which is what the numeric prefixes
    // are for: every module shares one scope and later definitions win.
    modules.sort();

    let mut table = String::from(
        "// Generated by build.rs. The agent, minified, one entry per module.\n",
    );
    table.push_str(&format!("pub static ORIGIN: &str = {:?};\n", origin));
    table.push_str(
        "pub static ADDR: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/agent/addr.js\"));\n",
    );
    table.push_str("pub static MODULES: &[(&str, &str)] = &[\n");

    let mut sites = Vec::new();
    for path in &modules {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let source = read(path);
        // The manifest is read before the module is minified, off the code the
        // author wrote, so the names in it are the names in the checkout.
        let found = js::sites(&source);
        if !found.is_empty() {
            sites.push(serde_json::json!({
                "module": module(&name),
                "hooks": found,
            }));
        }
        fs::write(staged.join(&name), js::compact(&source))
            .unwrap_or_else(|err| panic!("Could not stage {name}: {err}"));
        table.push_str(&format!(
            "    ({name:?}, include_str!(concat!(env!(\"OUT_DIR\"), \"/agent/{name}\"))),\n"
        ));
    }
    table.push_str("];\n");
    table.push_str(
        "pub static SITES: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/agent/sites.json\"));\n",
    );

    fs::write(
        staged.join("sites.json"),
        serde_json::to_string(&serde_json::Value::Array(sites)).expect("sites"),
    )
    .expect("could not stage the hook manifest");
    fs::write(out.join("agent.rs"), table).expect("could not write agent.rs");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("Could not read {}: {err}", path.display()))
}

/// The file name without its ordering prefix: 50-health.js is health, the same
/// name `--skip` takes.
fn module(name: &str) -> &str {
    name.trim_end_matches(".js")
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-')
}
