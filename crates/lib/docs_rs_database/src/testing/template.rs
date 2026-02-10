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
    #[instrument(skip(config))]
    pub async fn instance(config: &Config) -> Result<&'static Self> {
        TEMPLATE.get_or_try_init(|| Self::new(config)).await
    }

    #[instrument(skip(config))]
    async fn new(config: &Config) -> Result<Self> {
        // generate a database url without a database name to connect to the server
        // without connecting to a specific database.
        let mut base_url: Url = config.database_url.clone();
        base_url.set_path("/");

        let template_name = format!(
            "{}_template",
            config
                .original_db_name
                .as_deref()
                .unwrap_or_else(|| "docs_rs")
        );
        let mut template_url = base_url.clone();
        template_url.set_path(&format!("/{template_name}"));

        let mut admin_conn = sqlx::PgConnection::connect(base_url.as_str()).await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(LOCK_KEY)
            .execute(&mut admin_conn)
            .await?;

        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&template_name)
                .fetch_one(&mut admin_conn)
                .await?;

        if !exists {
            sqlx::query(&format!("CREATE DATABASE {template_name}"))
                .execute(&mut admin_conn)
                .await?;
        }

        let mut template_conn = sqlx::PgConnection::connect(template_url.as_str()).await?;
        migrations::migrate(&mut template_conn, None)
            .await
            .context("error running migrations")?;

        Ok(TemplateDatabase {
            base_url,
            template_name,
        })
    }
}
