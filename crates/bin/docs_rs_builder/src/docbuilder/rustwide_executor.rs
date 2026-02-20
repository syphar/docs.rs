use crate::{Config, docbuilder::build_error::RustwideBuildError, utils::copy::copy_dir_all};
use anyhow::{Context as _, Error, Result, anyhow};
use docs_rs_cargo_metadata::CargoMetadata;
use docs_rs_rustdoc_json::{
    RUSTDOC_JSON_COMPRESSION_ALGORITHMS, RustdocJsonFormatVersion,
    read_format_version_from_rustdoc_json,
};
use docs_rs_storage::{Storage, compress, get_file_list, rustdoc_json_path};
use docs_rs_types::{BuildId, KrateName, Version, doc_coverage::DocCoverage};
use docs_rs_utils::rustc_version::parse_rustc_version;
use docsrs_metadata::{HOST_TARGET, Metadata};
use rustwide::{
    Build, Toolchain, Workspace,
    cmd::Command,
    logging::{self, LogStorage},
};
use std::{
    fs::{self, File},
    io::BufReader,
    path::Path,
    sync::Arc,
};
use tracing::{debug, error, info, info_span, instrument, log, warn};

pub(crate) fn load_metadata_from_rustwide(
    workspace: &Workspace,
    toolchain: &Toolchain,
    source_dir: &Path,
) -> Result<CargoMetadata> {
    let res = Command::new(workspace, toolchain.cargo())
        .args(&["metadata", "--format-version", "1"])
        .cd(source_dir)
        .log_output(false)
        .run_capture()?;
    let [metadata] = res.stdout_lines() else {
        anyhow::bail!("invalid output returned by `cargo metadata`")
    };
    CargoMetadata::load_from_metadata(metadata)
}

pub(crate) struct RustwideBuildExecutor<'a> {
    workspace: &'a Workspace,
    toolchain: &'a Toolchain,
    config: &'a Config,
    blocking_storage: Arc<Storage>,
    rustc_version: &'a str,
}

impl<'a> RustwideBuildExecutor<'a> {
    pub(crate) fn new(
        workspace: &'a Workspace,
        toolchain: &'a Toolchain,
        config: &'a Config,
        blocking_storage: Arc<Storage>,
        rustc_version: &'a str,
    ) -> Self {
        Self {
            workspace,
            toolchain,
            config,
            blocking_storage,
            rustc_version,
        }
    }

    #[instrument(skip(self, build))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_target(
        &self,
        build_id: BuildId,
        name: &KrateName,
        version: &Version,
        target: &str,
        build: &Build,
        limits: &docs_rs_build_limits::Limits,
        local_storage: &Path,
        successful_targets: &mut Vec<String>,
        metadata: &Metadata,
        collect_metrics: bool,
    ) -> Result<FullBuildResult> {
        let target_res = self.execute_build(
            build_id,
            name,
            version,
            target,
            false,
            build,
            limits,
            metadata,
            false,
            collect_metrics,
        )?;
        if target_res.successful() {
            // Cargo is not giving any error and not generating documentation of some crates
            // when we use a target compile options. Check documentation exists before
            // adding target to successfully_targets.
            if build.host_target_dir().join(target).join("doc").is_dir() {
                debug!("adding documentation for target {} to the database", target,);
                self.copy_docs(&build.host_target_dir(), local_storage, target, false)?;
                successful_targets.push(target.to_string());
            }
        }
        Ok(target_res)
    }

    /// Run the build with rustdoc JSON output for a specific target and directly upload the
    /// build log & the JSON files.
    ///
    /// The method only returns an `Err` for internal errors that should be retryable.
    /// For all build errors we would just upload the log file and still return `Ok(())`.
    #[instrument(skip(self, build))]
    #[allow(clippy::too_many_arguments)]
    fn execute_json_build(
        &self,
        build_id: BuildId,
        name: &KrateName,
        version: &Version,
        target: &str,
        is_default_target: bool,
        build: &Build,
        metadata: &Metadata,
        limits: &docs_rs_build_limits::Limits,
    ) -> Result<()> {
        let rustdoc_flags = vec!["--output-format".to_string(), "json".to_string()];

        let mut storage = LogStorage::new(log::LevelFilter::Info);
        storage.set_max_size(limits.max_log_size());

        let result = logging::capture(&storage, || {
            let _span = info_span!("cargo_build_json", target = %target).entered();
            self.prepare_command(build, target, metadata, limits, rustdoc_flags, false)
                .and_then(|command| command.run().map_err(Into::into))
        });
        let successful = result.is_ok();

        {
            let _span = info_span!("store_json_build_logs").entered();
            let build_log_path = format!("build-logs/{build_id}/{target}_json.txt");
            self.blocking_storage
                .store_one(build_log_path, storage.to_string())
                .context("storing build log on S3")?;
        }

        if !successful {
            // this is a normal build error and will be visible in the uploaded build logs.
            // We don't need the Err variant here.
            return Ok(());
        }

        let json_dir = if metadata.proc_macro {
            assert!(
                is_default_target && target == HOST_TARGET,
                "can't handle cross-compiling macros"
            );
            build.host_target_dir().join("doc")
        } else {
            build.host_target_dir().join(target).join("doc")
        };

        let json_filename = fs::read_dir(&json_dir)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.is_file() && path.extension()? == "json" {
                    Some(path)
                } else {
                    None
                }
            })
            .next()
            .ok_or_else(|| {
                anyhow!(
                    "no JSON file found in target/doc after successful rustdoc json build.\n\
                     search directory: {}\n\
                     files: {:?}",
                    json_dir.to_string_lossy(),
                    get_file_list(&json_dir)
                        .filter_map(Result::ok)
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>(),
                )
            })?;

        let format_version = {
            let _span = info_span!("read_format_version").entered();
            read_format_version_from_rustdoc_json(&File::open(&json_filename)?)
                .context("couldn't parse rustdoc json to find format version")?
        };

        for alg in RUSTDOC_JSON_COMPRESSION_ALGORITHMS {
            let compressed_json: Vec<u8> = {
                let _span =
                    info_span!("compress_json", file_size = json_filename.metadata()?.len(), algorithm=%alg)
                        .entered();

                compress(BufReader::new(File::open(&json_filename)?), *alg)?
            };

            for format_version in [format_version, RustdocJsonFormatVersion::Latest] {
                let path = rustdoc_json_path(name, version, target, format_version, Some(*alg));
                let _span =
                    info_span!("store_json", %format_version, algorithm=%alg, target_path=%path)
                        .entered();

                self.blocking_storage
                    .store_one_uncompressed(&path, compressed_json.clone())?;
            }
        }

        Ok(())
    }

    #[instrument(skip(self, build))]
    fn get_coverage(
        &self,
        target: &str,
        build: &Build,
        metadata: &Metadata,
        limits: &docs_rs_build_limits::Limits,
    ) -> Result<Option<DocCoverage>> {
        let rustdoc_flags = vec![
            "--output-format".to_string(),
            "json".to_string(),
            "--show-coverage".to_string(),
        ];

        let mut coverage = DocCoverage {
            total_items: 0,
            documented_items: 0,
            total_items_needing_examples: 0,
            items_with_examples: 0,
        };

        self.prepare_command(build, target, metadata, limits, rustdoc_flags, false)?
            .process_lines(&mut |line, _| {
                if line.starts_with('{') && line.ends_with('}') {
                    match docs_rs_types::doc_coverage::parse_line(line) {
                        Ok(file_coverages) => coverage.extend(file_coverages),
                        Err(err) => warn!(?err, line, "failed to parse coverage line"),
                    }
                }
            })
            .log_output(true)
            .run()?;

        Ok(
            if coverage.total_items == 0 && coverage.documented_items == 0 {
                None
            } else {
                Some(coverage)
            },
        )
    }

    #[instrument(skip(self, build))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_build(
        &self,
        build_id: BuildId,
        name: &KrateName,
        version: &Version,
        target: &str,
        is_default_target: bool,
        build: &Build,
        limits: &docs_rs_build_limits::Limits,
        metadata: &Metadata,
        create_essential_files: bool,
        collect_metrics: bool,
    ) -> Result<FullBuildResult> {
        let cargo_metadata =
            load_metadata_from_rustwide(self.workspace, self.toolchain, &build.host_source_dir())?;

        let mut rustdoc_flags = vec![
            if create_essential_files {
                "--emit=toolchain-shared-resources"
            } else {
                "--emit=invocation-specific"
            }
            .to_string(),
        ];
        rustdoc_flags.extend(vec![
            "--resource-suffix".to_string(),
            format!("-{}", parse_rustc_version(self.rustc_version)?),
        ]);

        let mut storage = LogStorage::new(log::LevelFilter::Info);
        storage.set_max_size(limits.max_log_size());

        // we have to run coverage before the doc-build because currently it
        // deletes the doc-target folder.
        // https://github.com/rust-lang/cargo/issues/9447
        let doc_coverage = match self.get_coverage(target, build, metadata, limits) {
            Ok(cov) => cov,
            Err(err) => {
                info!("error when trying to get coverage: {}", err);
                info!("continuing anyways.");
                None
            }
        };

        if let Err(err) = self.execute_json_build(
            build_id,
            name,
            version,
            target,
            is_default_target,
            build,
            metadata,
            limits,
        ) {
            // FIXME: this is temporary. Theoretically all `Err` things coming out
            // of the method should be retryable, so we could juse use `?` here.
            // But since this is new, I want to be carful and first see what kind of
            // errors we are seeing here.
            error!(
                ?err,
                "internal error when trying to generate rustdoc JSON output"
            );
        }

        let result = {
            let _span = info_span!("cargo_build", target = %target, is_default_target).entered();
            logging::capture(&storage, || {
                self.prepare_command(
                    build,
                    target,
                    metadata,
                    limits,
                    rustdoc_flags,
                    collect_metrics,
                )
                .and_then(|command| command.run().map_err(Into::into))
            })
        };

        if collect_metrics
            && let Some(compiler_metric_target_dir) = &self.config.compiler_metrics_collection_path
        {
            let metric_output = build.host_target_dir().join("metrics/");
            info!(
                "found {} files in metric dir, copy over to {} (exists: {})",
                fs::read_dir(&metric_output)?.count(),
                &compiler_metric_target_dir.to_string_lossy(),
                &compiler_metric_target_dir.exists(),
            );
            copy_dir_all(&metric_output, compiler_metric_target_dir)?;
            fs::remove_dir_all(&metric_output)?;
        }

        // For proc-macros, cargo will put the output in `target/doc`.
        // Move it to the target-specific directory for consistency with other builds.
        // NOTE: don't rename this if the build failed, because `target/doc` won't exist.
        if result.is_ok() && metadata.proc_macro {
            assert!(
                is_default_target && target == HOST_TARGET,
                "can't handle cross-compiling macros"
            );
            // mv target/doc target/$target/doc
            let target_dir = build.host_target_dir();
            let old_dir = target_dir.join("doc");
            let new_dir = target_dir.join(target).join("doc");
            debug!("rename {} to {}", old_dir.display(), new_dir.display());
            std::fs::create_dir(target_dir.join(target))?;
            std::fs::rename(old_dir, new_dir)?;
        }

        Ok(FullBuildResult {
            result: BuildResult {
                rustc_version: self.rustc_version.to_string(),
                docsrs_version: format!("docsrs {}", docs_rs_utils::BUILD_VERSION),
                build_error: result.err(),
            },
            doc_coverage,
            cargo_metadata,
            build_log: storage.to_string(),
            target: target.to_string(),
        })
    }

    fn prepare_command<'ws, 'pl>(
        &self,
        build: &'ws Build,
        target: &str,
        metadata: &Metadata,
        limits: &docs_rs_build_limits::Limits,
        mut rustdoc_flags_extras: Vec<String>,
        collect_metrics: bool,
    ) -> Result<Command<'ws, 'pl>, RustwideBuildError> {
        // Add docs.rs specific arguments
        let mut cargo_args = vec![
            "--offline".into(),
            // We know that `metadata` unconditionally passes `-Z rustdoc-map`.
            // Don't copy paste this, since that fact is not stable and may change in the future.
            "-Zunstable-options".into(),
            // Add `target` so that if a dependency has target-specific docs, this links to them properly.
            //
            // Note that this includes the target even if this is the default, since the dependency
            // may have a different default (and the web backend will take care of redirecting if
            // necessary).
            //
            // FIXME: host-only crates like proc-macros should probably not have this passed? but #1417 should make it OK
            format!(
                r#"--config=doc.extern-map.registries.crates-io=\"https://docs.rs/{{pkg_name}}/{{version}}/{target}\""#
            ),
            // Enables the unstable rustdoc-scrape-examples feature. We are "soft launching" this feature on
            // docs.rs, but once it's stable we can remove this flag.
            "-Zrustdoc-scrape-examples".into(),
        ];
        if let Some(cpu_limit) = self.config.build_cpu_limit {
            cargo_args.push(format!("-j{cpu_limit}"));
        }
        // Cargo has a series of frightening bugs around cross-compiling proc-macros:
        // - Passing `--target` causes RUSTDOCFLAGS to fail to be passed 🤦
        // - Passing `--target` will *create* `target/{target-name}/doc` but will put the docs in `target/doc` anyway
        // As a result, it's not possible for us to support cross-compiling proc-macros.
        // However, all these caveats unfortunately still apply when `{target-name}` is the host.
        // So, only pass `--target` for crates that aren't proc-macros.
        //
        // Originally, this had a simpler check `target != HOST_TARGET`, but *that* was buggy when `HOST_TARGET` wasn't the same as the default target.
        // Rather than trying to keep track of it all, only special case proc-macros, which are what we actually care about.
        if !metadata.proc_macro {
            cargo_args.push("--target".into());
            cargo_args.push(target.into());
        };

        #[rustfmt::skip]
        const UNCONDITIONAL_ARGS: &[&str] = &[
            "--static-root-path", "/-/rustdoc.static/",
            "--cap-lints", "warn",
            "--extern-html-root-takes-precedence",
        ];

        rustdoc_flags_extras.extend(UNCONDITIONAL_ARGS.iter().map(|&s| s.to_owned()));
        let mut cargo_args = metadata.cargo_args(&cargo_args, &rustdoc_flags_extras);

        // If the explicit target is not a tier one target, we need to install it.
        let has_build_std = cargo_args.windows(2).any(|args| {
            args[0].starts_with("-Zbuild-std")
                || (args[0] == "-Z" && args[1].starts_with("build-std"))
        }) || cargo_args.last().unwrap().starts_with("-Zbuild-std");
        if !docsrs_metadata::DEFAULT_TARGETS.contains(&target) && !has_build_std {
            // This is a no-op if the target is already installed.
            self.toolchain.add_target(self.workspace, target)?;
        }

        let mut command = build
            .cargo()
            .timeout(Some(limits.timeout()))
            .no_output_timeout(None);

        for (key, val) in metadata.environment_variables() {
            command = command.env(key, val);
        }

        if collect_metrics && self.config.compiler_metrics_collection_path.is_some() {
            // set the `./target/metrics/` directory inside the build container
            // as a target directory for the metric files.
            let flag = "-Zmetrics-dir=/opt/rustwide/target/metrics";

            // this is how we can reach it from outside the container.
            fs::create_dir_all(build.host_target_dir().join("metrics/")).map_err(Error::from)?;

            let rustdocflags = toml::Value::try_from(vec![flag])
                .expect("serializing a string should never fail")
                .to_string();
            cargo_args.push("--config".into());
            cargo_args.push(format!("build.rustdocflags={rustdocflags}"));
        }

        Ok(command.args(&cargo_args))
    }

    #[instrument(skip(self))]
    pub(crate) fn copy_docs(
        &self,
        target_dir: &Path,
        local_storage: &Path,
        target: &str,
        is_default_target: bool,
    ) -> Result<()> {
        let source = target_dir.join(target).join("doc");

        let mut dest = local_storage.to_path_buf();
        // only add target name to destination directory when we are copying a non-default target.
        // this is allowing us to host documents in the root of the crate documentation directory.
        // for example winapi will be available in docs.rs/winapi/$version/winapi/ for it's
        // default target: x86_64-pc-windows-msvc. But since it will be built under
        // target/x86_64-pc-windows-msvc we still need target in this function.
        if !is_default_target {
            dest = dest.join(target);
        }

        info!("copy {} to {}", source.display(), dest.display());
        copy_dir_all(source, dest).map_err(Into::into)
    }
}

pub(crate) struct FullBuildResult {
    pub(crate) result: BuildResult,
    pub(crate) target: String,
    pub(crate) cargo_metadata: CargoMetadata,
    pub(crate) doc_coverage: Option<DocCoverage>,
    pub(crate) build_log: String,
}

impl FullBuildResult {
    pub(crate) fn successful(&self) -> bool {
        self.result.successful()
    }
}

#[derive(Debug)]
pub(crate) struct BuildResult {
    pub(crate) rustc_version: String,
    pub(crate) docsrs_version: String,
    pub(crate) build_error: Option<RustwideBuildError>,
}

impl BuildResult {
    pub(crate) fn successful(&self) -> bool {
        self.build_error.is_none()
    }
}
