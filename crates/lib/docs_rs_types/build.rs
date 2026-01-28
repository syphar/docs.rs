use anyhow::{Context as _, Result, bail};
use std::{env, fs::File, io::Write as _, path::Path, process::Command};

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

    let mut lines: Vec<String> = String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    lines.sort_unstable();

    let mut target_list_file = File::create(out_dir.join("static_target_list.rs"))?;
    writeln!(target_list_file, "const STATIC_TARGET_LIST: &[&str] = &[")?;

    for line in lines {
        writeln!(target_list_file, r#"    "{}", "#, line)?;
    }

    writeln!(target_list_file, "];")?;

    target_list_file.sync_all()?;

    Ok(())
}
