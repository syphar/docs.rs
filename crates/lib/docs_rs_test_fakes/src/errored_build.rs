use anyhow::Result;
use docs_rs_types::{BuildError, BuildId, ReleaseId};

#[derive(bon::Builder)]
#[builder(on(_, into))]
pub struct FakeEarlyErrorBuild {
    #[builder(
        setters(
            name = error_internal,
            vis = ""
        ),
    )]
    error: Option<(String, String)>,
}

use fake_early_error_build_builder::{IsComplete, IsUnset, SetError, State};

impl<S: State> FakeEarlyErrorBuildBuilder<S> {
    pub fn error<E>(self, error: &E) -> FakeEarlyErrorBuildBuilder<SetError<S>>
    where
        S::Error: IsUnset,
        E: BuildError,
    {
        self.error_internal((error.to_string(), error.kind().to_string()))
    }

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

        docs_rs_database::releases::update_build_with_error_text(
            &mut *conn,
            build_id,
            self.error.clone(),
        )
        .await?;

        Ok(build_id)
    }
}
