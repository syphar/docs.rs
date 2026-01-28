use anyhow::{Context as _, Result, bail};
use std::{env, fs::File, io::Write as _, path::Path, process::Command};

type TargetList<'a> = phf_codegen::OrderedSet<'a, String>;

fn main() -> Result<()> {
    let out_dir = env::var("OUT_DIR").context("missing OUT_DIR")?;
    let out_dir = Path::new(&out_dir);

    let mut target_list = TargetList::new();

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

    let stdout = String::from_utf8(output.stdout)?;

    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
    {
        target_list.entry(line);
    }

    let mut target_list_file = File::create(out_dir.join("static_target_list.rs"))?;
    writeln!(
        &mut target_list_file,
        "pub static STATIC_TARGET_LIST: ::phf::OrderedSet<&'static str> = {};",
        target_list.build()
    )?;
    target_list_file.sync_all()?;

    Ok(())
}
