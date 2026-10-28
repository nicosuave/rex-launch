//! `rex-launch setup`: installs the Rex action that opens the popup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::launch;

const MODULE: &str = include_str!("../lua/rex-launch.lua");
const REQUIRE: &str = r#"require("rex-launch")"#;

/// Copies the Lua module beside Rex's init.lua, has init.lua load it (once),
/// and reloads Rex's configuration. Safe to run again, for example after an
/// upgrade, which refreshes the module.
pub fn run() -> Result<()> {
    let init = rex_init_path();
    let dir = init.parent().context("Rex's init.lua has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let module = dir.join("rex-launch.lua");
    std::fs::write(&module, MODULE).with_context(|| format!("writing {}", module.display()))?;
    println!("wrote {}", module.display());

    let text = std::fs::read_to_string(&init).unwrap_or_default();
    if text.contains(REQUIRE) {
        println!("{} already loads rex-launch", init.display());
    } else {
        let exe = std::env::current_exe().context("finding the rex-launch executable")?;
        std::fs::write(&init, with_require(&text, &exe)).with_context(|| format!("writing {}", init.display()))?;
        println!("added rex-launch to {}", init.display());
    }

    match launch::run(launch::rex(None).args(["config", "reload"])) {
        Ok(_) => println!("reloaded Rex: press Cmd+Shift+N, or run \"New Task…\" from the command palette"),
        Err(err) => println!("couldn't reload Rex ({err:#}); run `rex config reload`"),
    }
    Ok(())
}

/// `text` with a line that loads the module appended, set to run `exe`.
fn with_require(text: &str, exe: &Path) -> String {
    let separator = match text {
        "" => "",
        t if t.ends_with("\n\n") => "",
        t if t.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    // Rust's quoting of a path is also a valid Lua string.
    let bin = format!("{:?}", exe.display().to_string());
    format!("{text}{separator}-- New Task popup: https://github.com/nicosuave/rex-launch\n{REQUIRE}.setup{{ bin = {bin} }}\n")
}

/// Rex reads `$REX_CONFIG`, else `$XDG_CONFIG_HOME/rex/init.lua`, else
/// `~/.config/rex/init.lua`.
fn rex_init_path() -> PathBuf {
    if let Some(path) = std::env::var_os("REX_CONFIG").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config"));
    config.join("rex/init.lua")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_the_require_after_existing_config() {
        let text = with_require("rex.bind(\"cmd+d\", \"pane.split\")\n", Path::new("/opt/bin/rex-launch"));
        assert_eq!(
            text,
            "rex.bind(\"cmd+d\", \"pane.split\")\n\n-- New Task popup: https://github.com/nicosuave/rex-launch\nrequire(\"rex-launch\").setup{ bin = \"/opt/bin/rex-launch\" }\n"
        );
    }

    #[test]
    fn starts_an_empty_config() {
        let text = with_require("", Path::new("/a b/rex-launch"));
        assert!(text.starts_with("-- New Task popup"));
        assert!(text.ends_with("setup{ bin = \"/a b/rex-launch\" }\n"));
    }
}
