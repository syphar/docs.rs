use anyhow::Result;
use docs_rs_types::{BuildId, ReleaseId, SimpleBuildError};

#[derive(bon::Builder)]
#[builder(on(_, into))]
pub struct FakeEarlyErrorBuild {
    error: Option<SimpleBuildError>,
}

use fake_early_error_build_builder::{IsComplete, State};

impl<S: State> FakeEarlyErrorBuildBuilder<S> {
    pub async fn create(
        self,
        conn: &mut sqlx::PgConnection,
        release_id: ReleaseId,
    ) -> Result<BuildId>
    where
        S: IsComplete,
    {
        self.build().create(conn, release_id).await
    }
}

impl FakeEarlyErrorBuild {
    pub async fn create(
        &self,
        conn: &mut sqlx::PgConnection,
        release_id: ReleaseId,
    ) -> Result<BuildId> {
        let build_id = docs_rs_database::releases::initialize_build(&mut *conn, release_id).await?;

        docs_rs_database::releases::update_build_with_error(
            &mut *conn,
            build_id,
            self.error.as_ref(),
        )
        .await?;

        Ok(build_id)
    }
}
