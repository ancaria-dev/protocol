//! Bundles the agent's JavaScript into the single script Frida injects.
//!
//! The modules are linked into this executable by `build.rs`, minified and in
//! load order, and the bundle is assembled here in memory. Nothing writes
//! JavaScript to disk: there is no folder beside the host to unpack, to keep in
//! step with it, or to leave behind when it is replaced.
//!
//! Every module shares one scope, so load order matters and is encoded in the
//! file names: `gen/addr.js` first (the generated address table), then the
//! numbered sources in order. Two modules declaring the same helper silently
//! overwrite each other, which has cost a debugging round before.
//!
//! `--agent <path>` reads a folder instead, which is what somebody editing the
//! agent wants: change a file, restart the host, no rebuild.

use std::path::Path;

use anyhow::{Context, Result};

/// The table `build.rs` wrote: `ORIGIN`, `ADDR`, `MODULES`, and `SITES`.
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/agent.rs"));
}

/// Where the embedded agent came from: a checkout, or a pinned release.
pub fn origin() -> &'static str {
    embedded::ORIGIN
}

/// One module and its source, in load order.
type Module = (String, String);

/// Modules are named by their file name without the number prefix ("gold",
/// "position"). `skip` leaves those out, `only` loads nothing but those.
/// `core` and `bus` always load, since everything else depends on them.
/// Bisecting a crash otherwise means an edit and a rebuild per attempt, and
/// every attempt costs a game restart.
pub fn bundle(dir: Option<&Path>, skip: &[String], only: &[String],
              ask: bool, no_hook: &[String], trace: bool) -> Result<String> {
    let (mut out, sources) = read(dir)?;

    // Individual sites the agent must not attach to, read by core's hook().
    let names: Vec<String> = no_hook.iter().map(|n| format!("\"{n}\"")).collect();
    out.push_str(&format!("
var NO_HOOK = [{}];
", names.join(", ")));
    out.push_str(&format!("var HOOK_TRACE = {trace};
"));

    for (name, source) in sources {
        let module = module(&name);
        // names installs no hooks, it only wraps two of the game's lookups,
        // and health/session read through it, so leaving it out just breaks them.
        let required = matches!(module, "core" | "bus" | "names");
        let wanted = if only.is_empty() {
            !skip.iter().any(|s| s == module)
        } else {
            required || only.iter().any(|s| s == module)
        };
        if !wanted {
            println!("[host] Skipping agent module {module}");
            continue;
        }
        out.push_str(&format!("\n// ---- {name} ----\n"));
        out.push_str(&source);
    }
    if !ask {
        // Separates "the hook is installed" from "the hook stops the game
        // thread waiting for a verdict".  One game restart answers which.
        out.push_str("
// --no-ask
askEnabled = false;
");
    }
    Ok(out)
}

/// The hook sites the agent installs, as the JSON the launcher reads: one
/// entry per module that attaches to something, in load order.
///
/// Written by the build off the sources before they were minified, so the
/// names in it are the names in the checkout. The launcher asks for it with
/// `protocol.exe --hooks` rather than reading the scripts, which it no longer
/// has: a site nobody can see is a site nobody can switch off.
pub fn manifest(dir: Option<&Path>) -> Result<String> {
    let Some(dir) = dir else {
        return Ok(embedded::SITES.to_string());
    };
    let (_, sources) = read(Some(dir))?;
    let groups: Vec<serde_json::Value> = sources
        .iter()
        .filter_map(|(name, source)| {
            let sites = crate::js::sites(source);
            if sites.is_empty() {
                return None;
            }
            Some(serde_json::json!({ "module": module(name), "hooks": sites }))
        })
        .collect();
    Ok(serde_json::Value::Array(groups).to_string())
}

/// The address table and every module, either linked in or read off disk.
fn read(dir: Option<&Path>) -> Result<(String, Vec<Module>)> {
    let Some(dir) = dir else {
        let modules = embedded::MODULES
            .iter()
            .map(|(name, source)| (name.to_string(), source.to_string()))
            .collect();
        return Ok((embedded::ADDR.to_string(), modules));
    };

    let generated = dir.join("gen/addr.js");
    let addr = std::fs::read_to_string(&generated).with_context(|| {
        format!(
            "{} is missing—run `python tools/addr.py` in the Coderpack checkout",
            generated.display()
        )
    })?;

    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("Could not read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "js"))
        .collect();
    paths.sort();

    let mut modules = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        modules.push((name, std::fs::read_to_string(&path)?));
    }
    Ok((addr, modules))
}

/// The file name without its ordering prefix: 50-health.js is health, the same
/// name `--skip` takes.
fn module(name: &str) -> &str {
    name.trim_end_matches(".js")
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// A small agent of this repository's own, not the real one next door.
    /// What these tests check is the bundler: load order, which modules
    /// survive a filter, and where NO_HOOK is declared. Pointing them at
    /// coderpack made `cargo test` fail in a lone clone and coupled this
    /// repository's test suite to another repository's file names.
    fn agent_dir() -> PathBuf {
        PathBuf::from("tests/agent")
    }

    #[test]
    fn disabled_sites_reach_the_agent() {
        let bundle = super::bundle(Some(&agent_dir()), &[], &[], true,
                                   &["goldEpilogue".to_string()], false)
            .expect("bundles");
        assert!(bundle.contains("var NO_HOOK = [\"goldEpilogue\"];"),
                "the agent must be told which sites to leave alone");
        assert!(bundle.find("var NO_HOOK").unwrap()
                    < bundle.find("function hook(").unwrap(),
                "NO_HOOK has to be declared before core reads it");
    }

    #[test]
    fn only_keeps_the_runtime_modules() {
        let bundle = super::bundle(Some(&agent_dir()), &[], &["gold".to_string()],
                                   true, &[], false).expect("bundles");
        assert!(bundle.contains("60-gold.js"));
        assert!(bundle.contains("10-core.js"), "core is not optional");
        assert!(!bundle.contains("50-health.js"));
    }

    /// The real agent, as it is linked into this executable. A build that found
    /// no source at all would fail rather than reach here, so what is being
    /// checked is that the order and the filters still hold on the real
    /// modules, and that the address table came with them.
    #[test]
    fn the_embedded_agent_is_loadable() {
        let bundle = super::bundle(None, &[], &[], true, &[], false).expect("bundles");
        assert!(bundle.contains("var RVA"), "the address table comes first");
        assert!(bundle.find("var RVA").unwrap() < bundle.find("var NO_HOOK").unwrap());
        assert!(bundle.find("var NO_HOOK").unwrap()
                    < bundle.find("// ---- 10-core.js ----").unwrap(),
                "core reads NO_HOOK, so it is declared before core");
    }

    /// Minification is comments and layout, and nothing else. Whole lines of
    /// comment are the bulk of it, so their absence is what is measured.
    #[test]
    fn the_embedded_agent_is_minified() {
        let commented = super::embedded::MODULES
            .iter()
            .flat_map(|(_, source)| source.lines())
            .any(|line| line.starts_with("//") || line.starts_with(" "));
        assert!(!commented, "the embedded modules keep comments or indentation");
    }

    /// Every module the real agent has is still a module, and the ones the
    /// filters must never drop are still there.
    #[test]
    fn the_embedded_agent_keeps_its_required_modules() {
        let names: Vec<&str> = super::embedded::MODULES
            .iter()
            .map(|(name, _)| super::module(name))
            .collect();
        for required in ["core", "bus", "names"] {
            assert!(names.contains(&required), "{required} is missing from the bundle");
        }
    }

    /// What the launcher draws its hook list from.
    #[test]
    fn the_manifest_names_modules_and_sites() {
        let manifest = super::manifest(None).expect("manifest");
        let groups: serde_json::Value = serde_json::from_str(&manifest).expect("json");
        let groups = groups.as_array().expect("an array of groups");
        assert!(!groups.is_empty(), "no hook sites were read out of the agent");
        assert!(groups.iter().any(|group| group["module"] == "core"));
        assert!(groups.iter().all(|group| {
            group["hooks"].as_array().is_some_and(|hooks| !hooks.is_empty())
        }), "a module with no sites has no row");
    }

    /// The same list, read out of a folder: what `--agent` gets.
    #[test]
    fn a_folder_has_a_manifest_too() {
        let manifest = super::manifest(Some(&agent_dir())).expect("manifest");
        assert!(manifest.contains("\"gold\""), "got {manifest}");
        assert!(manifest.contains("goldDelta"), "got {manifest}");
    }
}
