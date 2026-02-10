use crate::{Config, migrations};
use anyhow::{Context as _, Result, anyhow};
use sqlx::Connection as _;
use tokio::sync::OnceCell;
use tracing::instrument;
use url::Url;

static TEMPLATE: OnceCell<TemplateDatabase> = OnceCell::const_new();
const LOCK_KEY: i64 = 0x5A17_F00D;

pub(super) struct TemplateDatabase {
    pub(super) base_url: Url,
    pub(super) template_name: String,
}

impl TemplateDatabase {
    #[instrument]
    pub async fn instance(config: &Config) -> Result<&'static Self> {
        TEMPLATE.get_or_try_init(|| Self::new(config)).await
    }

    #[instrument]
    async fn new(config: &Config) -> Result<Self> {
        let mut base_url: Url = config.database_url.parse()?;

        let prefix = base_url.path().strip_prefix('/');
        let prefix = prefix
            .ok_or_else(|| anyhow!("failed to parse database name"))?
            .to_string();
        base_url.set_path("/");

        let template_name = format!("{prefix}_template");
        let mut template_url = base_url.clone();
        template_url.set_path(&format!("/{template_name}"));

        let mut conn = sqlx::PgConnection::connect(base_url.as_str()).await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(LOCK_KEY)
            .execute(&mut conn)
            .await?;

        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&template_name)
                .fetch_one(&mut conn)
                .await?;

        if !exists {
            sqlx::query(&format!("CREATE DATABASE {template_name}"))
                .execute(&mut conn)
                .await?;

            drop(conn);

            let mut conn = sqlx::PgConnection::connect(template_url.as_str()).await?;
            migrations::migrate(&mut conn, None)
                .await
                .context("error running migrations")?;
        }

        Ok(TemplateDatabase {
            base_url,
            // pool,
            template_name,
            prefix,
        })
    }

    // #[instrument(skip(self))]
    // fn get_connection(&self) -> PooledConnection<ConnectionManager<PgConnection>> {
    //     self.pool.get().expect("Failed to get database connection")
    // }
}
