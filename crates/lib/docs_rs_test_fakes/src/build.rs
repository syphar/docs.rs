use anyhow::{Result, bail};
use docs_rs_database::releases::add_build_logs;
use docs_rs_storage::AsyncStorage;
use docs_rs_types::{BuildStatus, ReleaseId, SimpleBuildError};
use std::collections::HashMap;

#[derive(bon::Builder)]
#[builder(on(_, into))]
pub struct FakeBuild {
    #[builder(field)]
    other_build_logs: HashMap<String, (String, bool)>,

    #[builder(
        setters(
            name = s3_build_log_internal,
            vis = ""
        )
    )]
    s3_build_log: Option<(String, bool)>,

    db_build_log: Option<String>,

    rustc_version: Option<String>,

    docsrs_version: Option<String>,

    #[builder(default = BuildStatus::InProgress)]
    pub build_status: BuildStatus,

    memory_peak: Option<u64>,

    /// new build logs: we have a record in the `builds_logs` table for each log, including a status
    /// old build logs: people have to run `s3 ls` with prefix to know which build logs exist
    #[builder(default = false)]
    legacy_build_logs: bool,
}

use fake_build_builder::{IsComplete, IsUnset, SetBuildStatus, SetS3BuildLog, State};

use crate::build::fake_build_builder::{SetDocsrsVersion, SetMemoryPeak, SetRustcVersion};

impl<S: State> FakeBuildBuilder<S> {
    pub fn s3_build_log(
        self,
        build_log: impl Into<String>,
        successful: bool,
    ) -> FakeBuildBuilder<SetS3BuildLog<S>>
    where
        S::S3BuildLog: IsUnset,
    {
        self.s3_build_log_internal((build_log.into(), successful))
    }

    pub fn no_s3_build_log(self) -> FakeBuildBuilder<SetS3BuildLog<S>>
    where
        S::S3BuildLog: IsUnset,
    {
        self.maybe_s3_build_log_internal(None::<(String, bool)>)
    }

    pub fn build_log_for_other_target(
        mut self,
        target: impl Into<String>,
        build_log: impl Into<String>,
        successful: bool,
    ) -> Self {
        self.other_build_logs
            .insert(target.into(), (build_log.into(), successful));
        self
    }

    pub fn successful(self, successful: bool) -> FakeBuildBuilder<SetBuildStatus<S>>
    where
        S::BuildStatus: IsUnset,
    {
        self.build_status(if successful {
            BuildStatus::Success
        } else {
            BuildStatus::Failure
        })
    }

    pub async fn create(
        self,
        conn: &mut sqlx::PgConnection,
        storage: &AsyncStorage,
        release_id: ReleaseId,
        default_target: &str,
    ) -> Result<()>
    where
        S: IsComplete,
    {
        self.build()
            .create(conn, storage, release_id, default_target)
            .await
    }
}

impl FakeBuild {
    pub fn default<S>() -> FakeBuildBuilder<
        SetMemoryPeak<SetBuildStatus<SetDocsrsVersion<SetRustcVersion<SetS3BuildLog>>>>,
    > {
        FakeBuild::builder()
            .s3_build_log("It works!", true)
            .rustc_version("rustc 2.0.0-nightly (000000000 1970-01-01)")
            .docsrs_version("docs.rs 1.0.0 (000000000 1970-01-01)")
            .build_status(BuildStatus::Success)
            .memory_peak(23u64)
    }

    pub async fn create(
        &self,
        conn: &mut sqlx::PgConnection,
        storage: &AsyncStorage,
        release_id: ReleaseId,
        default_target: &str,
    ) -> Result<()> {
        let build_id = docs_rs_database::releases::initialize_build(&mut *conn, release_id).await?;

        docs_rs_database::releases::finish_build(
            &mut *conn,
            build_id,
            &self.rustc_version,
            &self.docsrs_version,
            self.build_status,
            Some(42),
            self.memory_peak,
            None::<&SimpleBuildError>,
        )
        .await?;

        if let Some(db_build_log) = self.db_build_log.as_deref() {
            sqlx::query!(
                "UPDATE builds SET output = $2 WHERE id = $1",
                build_id.0,
                db_build_log
            )
            .execute(&mut *conn)
            .await?;
        }

        let prefix = format!("build-logs/{build_id}/");

        let mut log_filenames = Vec::new();

        if let Some((s3_build_log, successful)) = &self.s3_build_log {
            log_filenames.push((format!("{default_target}.txt"), *successful));
            storage
                .store_one(
                    format!("{prefix}{default_target}.txt"),
                    s3_build_log.clone(),
                )
                .await?;
        }

        for (target, (log, successful)) in &self.other_build_logs {
            if target == default_target {
                bail!("build log for default target has to be set via `s3_build_log`");
            }
            log_filenames.push((format!("{target}.txt"), *successful));
            storage
                .store_one(format!("{prefix}{target}.txt"), log.clone())
                .await?;
        }

        if !self.legacy_build_logs && !log_filenames.is_empty() {
            add_build_logs(&mut *conn, build_id, log_filenames).await?;
        }

        Ok(())
    }
}
