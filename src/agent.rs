//! Bundles the agent's JavaScript into the single script Frida injects.
//!
//! Every module shares one scope, so load order matters and is encoded in the
//! file names: `gen/addr.js` first (the generated address table), then the
//! numbered sources in order.  Two modules declaring the same helper silently
//! overwrite each other, which has cost a debugging round before.

use std::path::Path;

use anyhow::{Context, Result};

/// Modules are named by their file name without the number prefix ("gold",
/// "position").  `skip` leaves those out, `only` loads nothing but those.
/// `core` and `bus` always load, since everything else depends on them.
/// Bisecting a crash otherwise means an edit and a rebuild per attempt, and
/// every attempt costs a game restart.
pub fn bundle(dir: &Path, skip: &[String], only: &[String],
              ask: bool, no_hook: &[String], trace: bool) -> Result<String> {
    let generated = dir.join("gen/addr.js");
    let mut out = std::fs::read_to_string(&generated).with_context(|| {
        format!(
            "{} is missing—run `python tools/addr.py` in the Coderpack checkout",
            generated.display()
        )
    })?;

    // Individual sites the agent must not attach to, read by core's hook().
    let names: Vec<String> = no_hook.iter().map(|n| format!("\"{n}\"")).collect();
    out.push_str(&format!("
var NO_HOOK = [{}];
", names.join(", ")));
    out.push_str(&format!("var HOOK_TRACE = {trace};
"));

    let mut sources: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("Could not read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "js"))
        .collect();
    sources.sort();

    for path in sources {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let module = name.trim_end_matches(".js")
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
        // names installs no hooks, it only wraps two of the game's lookups --
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
        out.push_str(&std::fs::read_to_string(&path)?);
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
        let bundle = super::bundle(&agent_dir(), &[], &[], true,
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
        let bundle = super::bundle(&agent_dir(), &[], &["gold".to_string()],
                                   true, &[], false).expect("bundles");
        assert!(bundle.contains("60-gold.js"));
        assert!(bundle.contains("10-core.js"), "core is not optional");
        assert!(!bundle.contains("50-health.js"));
    }
}
