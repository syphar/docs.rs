use anyhow::{Context as _, Result, bail};
use std::{collections::BTreeSet, env, fs::File, io::Write as _, path::Path, process::Command};

fn main() -> Result<()> {
    let out_dir = env::var("OUT_DIR").context("missing OUT_DIR")?;
    let out_dir = Path::new(&out_dir);

    let output = Command::new("rustc")
        .arg("--print")
        .arg("target-list")
        .output()?;

    if !output.status.success() {
        bail!(
            "`rustc --print target-list` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut lines: BTreeSet<String> = String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();

    // add some legacy targets that existed in the past, but are gone now.
    for t in &[
        "wasm32-wasi",
        "x86_64-unknown-openbsd",
        "x86_64-unknown-none",
        "x86_64-unknown-uefi",
        "x86_64-windows-msvc",
    ] {
        lines.insert(t.to_string());
    }

    let mut target_list_file = File::create(out_dir.join("static_target_list.rs"))?;
    writeln!(target_list_file, "const STATIC_TARGET_LIST: &[&str] = &[")?;

    for line in lines {
        writeln!(target_list_file, r#"    "{}", "#, line)?;
    }

    writeln!(target_list_file, "];")?;

    target_list_file.sync_all()?;

    Ok(())
}
