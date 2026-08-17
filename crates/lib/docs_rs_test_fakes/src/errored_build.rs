use anyhow::Result;
use docs_rs_types::{BuildError, BuildId, ReleaseId, SimpleBuildError};

#[derive(bon::Builder)]
#[builder(
    on(_, into),
    generics(setters(
        name = "with_{}",
        vis = "",
    )),
    start_fn(vis = "", name = builder_internal),
)]
pub struct FakeEarlyErrorBuild<E> {
    #[builder(setters(name = error_internal, vis = ""))]
    error: Option<E>,
}

impl FakeEarlyErrorBuild<SimpleBuildError> {
    pub fn builder() -> FakeEarlyErrorBuildBuilder<SimpleBuildError> {
        Self::builder_internal()
    }
}

use fake_early_error_build_builder::{IsComplete, IsUnset, SetError, State};

impl<E, S> FakeEarlyErrorBuildBuilder<E, S>
where
    E: BuildError,
    S: State,
{
    pub fn error<NewE>(self, error: NewE) -> FakeEarlyErrorBuildBuilder<NewE, SetError<S>>
    where
        NewE: BuildError,
        S::Error: IsUnset,
    {
        self.with_e().error_internal(error)
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

impl<E> FakeEarlyErrorBuild<E>
where
    E: BuildError,
{
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
