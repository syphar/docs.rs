//! Stage a workspace without Cargo's publication-time manifest normalization.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use walkdir::WalkDir;

pub(crate) struct StagedSource {
    pub(crate) directory: tempfile::TempDir,
    pub(crate) directory_label: String,
    pub(crate) manifest_path: PathBuf,
}

pub(crate) fn create(
    crate_path: &Path,
    package: Option<&str>,
    build_workspace: &Path,
) -> Result<StagedSource> {
    let requested_manifest = crate_path.join("Cargo.toml").canonicalize()?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&requested_manifest)
        .current_dir(crate_path)
        .output()
        .context("reading source workspace metadata")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let root = Path::new(string(&metadata, "workspace_root")?).canonicalize()?;
    let packages = metadata["packages"]
        .as_array()
        .context("missing metadata packages")?;
    let members = metadata["workspace_members"]
        .as_array()
        .context("missing workspace members")?;
    let defaults = metadata["workspace_default_members"]
        .as_array()
        .context("missing default members")?;
    let requested: toml::Table = toml::from_str(&fs::read_to_string(&requested_manifest)?)?;
    if package.is_none() && !requested.contains_key("package") {
        bail!(
            "`{}` is a virtual workspace; select a member with `--package <NAME>`",
            requested_manifest.display()
        );
    }
    let selected: Vec<_> = packages
        .iter()
        .filter(|p| {
            members.contains(&p["id"])
                && match package {
                    Some(name) => p["name"].as_str() == Some(name),
                    None if requested_manifest == root.join("Cargo.toml") => {
                        defaults.contains(&p["id"])
                    }
                    None => p["manifest_path"]
                        .as_str()
                        .is_some_and(|path| Path::new(path) == requested_manifest),
                }
        })
        .collect();
    let [selected] = selected.as_slice() else {
        bail!(
            "source builds require exactly one workspace member; use --package <NAME> (matched {})",
            selected.len()
        );
    };
    let manifest = Path::new(string(selected, "manifest_path")?).canonicalize()?;
    let manifest_path = manifest
        .strip_prefix(&root)
        .context("selected package is outside the workspace")?
        .to_owned();
    let mut manifests: Vec<PathBuf> = packages
        .iter()
        .filter_map(|p| p["manifest_path"].as_str().map(PathBuf::from))
        .chain(std::iter::once(root.join("Cargo.toml")))
        .collect();
    let mut staged_manifests = std::collections::HashMap::new();
    while let Some(path) = manifests.pop() {
        let path = path.canonicalize()?;
        if staged_manifests.contains_key(&path) {
            continue;
        }
        ensure!(
            path.starts_with(&root),
            "manifest {} is outside the source workspace",
            path.display()
        );
        let original = fs::read_to_string(&path)?;
        let mut value: toml::Value = toml::from_str(&original)?;
        let mut changed = rebase_paths(
            &mut value,
            path.parent().unwrap(),
            &root,
            false,
            &mut manifests,
        )?;
        if path == root.join("Cargo.toml") && value.get("workspace").is_none() {
            value
                .as_table_mut()
                .context("invalid source manifest")?
                .insert("workspace".into(), toml::Value::Table(toml::Table::new()));
            changed = true;
        }
        staged_manifests.insert(
            path,
            if changed {
                toml::to_string(&value)?
            } else {
                original
            },
        );
    }
    let directory_label = format!(
        "{}-{}",
        string(selected, "name")?,
        string(selected, "version")?
    );
    let target = Path::new(string(&metadata, "target_directory")?);
    let build_workspace = build_workspace
        .canonicalize()
        .unwrap_or_else(|_| build_workspace.to_owned());
    ensure!(
        build_workspace != root,
        "the build workspace must not be the source workspace root"
    );
    let directory = tempfile::tempdir().context("creating source staging directory")?;
    tracing::info!(workspace = %root.display(), "staging local workspace source");
    let entries = WalkDir::new(&root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || (!matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | ".workspace" | ".rustwide-docker")
                ) && entry.path() != target
                    && entry.path() != build_workspace)
        });
    for entry in entries {
        let entry = entry.context("walking source workspace")?;
        let canonical = entry.path().canonicalize()?;
        ensure!(
            canonical.starts_with(&root),
            "source path `{}` points outside the workspace; external source paths are not supported",
            entry.path().display()
        );
        let relative = entry.path().strip_prefix(&root)?;
        let destination = directory.path().join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_file() {
            fs::copy(entry.path(), &destination)
                .with_context(|| format!("staging {}", entry.path().display()))?;
            if let Some(manifest) = staged_manifests.get(&canonical) {
                fs::write(&destination, manifest)?;
            }
        }
    }
    ensure!(
        directory.path().join(&manifest_path).is_file(),
        "selected manifest was excluded from the source copy"
    );
    Ok(StagedSource {
        directory,
        directory_label,
        manifest_path,
    })
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing metadata {key}"))
}

fn rebase_paths(
    value: &mut toml::Value,
    manifest_dir: &Path,
    root: &Path,
    dependency: bool,
    manifests: &mut Vec<PathBuf>,
) -> Result<bool> {
    let Some(table) = value.as_table_mut() else {
        return Ok(false);
    };
    let mut changed = false;
    if dependency
        && let Some(path) = table.get_mut("path")
        && let Some(original) = path.as_str()
    {
        let resolved = manifest_dir
            .join(original)
            .canonicalize()
            .with_context(|| {
                format!(
                    "resolving path dependency {original} in {}",
                    manifest_dir.display()
                )
            })?;
        let relative = resolved.strip_prefix(root).with_context(|| format!("path dependency `{original}` in `{}` is outside the workspace; external path dependencies are not supported by --source-build", manifest_dir.display()))?;
        manifests.push(resolved.join("Cargo.toml"));
        if Path::new(original).is_absolute() {
            let mut rebased = PathBuf::new();
            for _ in manifest_dir.strip_prefix(root)?.components() {
                rebased.push("..");
            }
            rebased.push(relative);
            *path = toml::Value::String(rebased.to_string_lossy().into_owned());
            changed = true;
        }
    }
    for (key, child) in table.iter_mut() {
        if !dependency && matches!(key.as_str(), "package" | "metadata") {
            continue;
        }
        changed |= rebase_paths(
            child,
            manifest_dir,
            root,
            dependency
                || matches!(
                    key.as_str(),
                    "dependencies"
                        | "dev-dependencies"
                        | "build-dependencies"
                        | "patch"
                        | "replace"
                ),
            manifests,
        )?;
    }
    Ok(changed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            r#"
[workspace]
members = ["selected", "sibling"]
resolver = "2"
[workspace.package]
version = "0.1.0"
edition = "2021"
[workspace.dependencies]
sibling = { path = "sibling" }
"#,
        )
        .unwrap();
        for name in ["selected", "sibling"] {
            fs::create_dir_all(root.path().join(name).join("src")).unwrap();
            fs::write(
                root.path().join(name).join("Cargo.toml"),
                format!(
                    r#"
[package]
name = "{name}"
version.workspace = true
edition.workspace = true
publish = false
"#
                ),
            )
            .unwrap();
        }
        fs::write(root.path().join("selected/Cargo.toml"), format!("{}\n[dependencies]\nsibling.workspace = true\n[package.metadata.docs.rs]\nfeatures = [\"documented\"]\n[features]\ndocumented = []\n", fs::read_to_string(root.path().join("selected/Cargo.toml")).unwrap())).unwrap();
        fs::write(root.path().join("selected/src/lib.rs"), "#[cfg(not(feature = \"documented\"))] compile_error!(\"missing selected metadata\");\npub use sibling::LocalOnly;\n").unwrap();
        fs::write(
            root.path().join("sibling/src/lib.rs"),
            "pub struct LocalOnly;\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn preserves_workspace_and_local_dependencies_without_touching_checkout() -> Result<()> {
        let original = fixture();
        let root = original.path();
        for cache in ["target", "selected/target", ".git", "custom-cache"] {
            fs::create_dir_all(root.join(cache))?;
            fs::write(root.join(cache).join("sentinel"), "do not copy")?;
        }
        let source = create(root, Some("selected"), &root.join("custom-cache"))?;
        assert_eq!(source.manifest_path, Path::new("selected/Cargo.toml"));
        for cache in ["target", "selected/target", ".git", "custom-cache"] {
            assert!(!source.directory.path().join(cache).exists());
        }
        let output = Command::new("cargo")
            .args([
                "doc",
                "--offline",
                "--no-deps",
                "--features",
                "documented",
                "--manifest-path",
                "selected/Cargo.toml",
            ])
            .current_dir(source.directory.path())
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            source
                .directory
                .path()
                .join("target/doc/selected/index.html")
                .exists()
        );
        assert!(!root.join("Cargo.lock").exists());
        assert_eq!(
            fs::read(root.join("Cargo.toml"))?,
            fs::read(source.directory.path().join("Cargo.toml"))?
        );
        let direct = create(&root.join("selected"), None, &root.join("target/cache"))?;
        assert_eq!(direct.manifest_path, source.manifest_path);
        assert!(create(root, None, &root.join("target/cache")).is_err());
        assert!(create(root, Some("missing"), &root.join("target/cache")).is_err());
        Ok(())
    }

    #[test]
    fn isolates_standalone_sources_and_honors_default_members() -> Result<()> {
        let original = fixture();
        let root = original.path();
        let root_manifest = root.join("Cargo.toml");
        let workspace = fs::read_to_string(&root_manifest)?;
        fs::create_dir(root.join("src"))?;
        fs::write(root.join("src/lib.rs"), "pub struct Root;\n")?;
        fs::write(
            &root_manifest,
            format!(
                "{workspace}\n[package]\nname = \"root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
            ),
        )?;
        // With a root package, Cargo defaults to it unless default-members override it.
        let staged = create(root, None, &root.join("target/cache"))?;
        assert_eq!(staged.manifest_path, Path::new("Cargo.toml"));
        let manifest = fs::read_to_string(&root_manifest)?;
        fs::write(
            &root_manifest,
            manifest.replace(
                "[workspace]",
                "[workspace]\ndefault-members = [\"selected\"]",
            ),
        )?;
        let staged = create(root, None, &root.join("target/cache"))?;
        assert_eq!(staged.manifest_path, Path::new("selected/Cargo.toml"));
        fs::write(
            &root_manifest,
            "[package]\nname = \"standalone\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        let staged = create(root, None, &root.join("target/cache"))?;
        let manifest: toml::Value = toml::from_str(&fs::read_to_string(
            staged.directory.path().join("Cargo.toml"),
        )?)?;
        assert!(manifest["workspace"].as_table().unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn rebases_absolute_dependencies_and_rejects_external_paths() -> Result<()> {
        let original = fixture();
        let root = original.path();
        let manifest = root.join("Cargo.toml");
        let text = fs::read_to_string(&manifest)?;
        fs::write(
            &manifest,
            text.replace(
                "path = \"sibling\"",
                &format!("path = {:?}", root.join("sibling")),
            ),
        )?;
        let staged = create(root, Some("selected"), &root.join("target/cache"))?;
        let staged_manifest: toml::Value = toml::from_str(&fs::read_to_string(
            staged.directory.path().join("Cargo.toml"),
        )?)?;
        assert_eq!(
            staged_manifest["workspace"]["dependencies"]["sibling"]["path"].as_str(),
            Some("sibling")
        );
        let external = tempfile::tempdir()?;
        let mut dependency: toml::Value = toml::from_str(&format!(
            "[dependencies.external]\npath = {:?}",
            external.path()
        ))?;
        assert!(
            rebase_paths(&mut dependency, root, root, false, &mut Vec::new())
                .unwrap_err()
                .to_string()
                .contains("outside the workspace")
        );
        Ok(())
    }
}
