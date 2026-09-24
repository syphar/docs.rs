use crate::{Config, ReplacementDetails, ReplacementMap};
use anyhow::Result;
use docs_rs_reqwest::{CachedResult, Client};
use docs_rs_types::KrateName;
use std::{sync::Arc, time::Duration};
use url::Url;

/// A single snapshot, fetched lazily and revalidated on demand using its ETag.
#[derive(Debug)]
pub struct StdReplacements {
    client: Client<ReplacementMap>,
    url: Url,
}

impl StdReplacements {
    /// Create a client without fetching data. Failed refreshes retain the last
    /// snapshot and defer retries for 30 seconds; initial-load failures propagate.
    pub fn from_config(config: &Config) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .max_retries(config.max_retries)
                .cache_capacity(1u64)
                .default_ttl(config.cache_default_ttl)
                .stale_if_error(Duration::from_secs(30))
                .build()?,
            url: config.url.clone(),
        })
    }

    /// Return the alternative for a crate, refreshing an expired snapshot if needed.
    pub async fn get(
        &self,
        name: &KrateName,
    ) -> Result<CachedResult<Option<Arc<ReplacementDetails>>>> {
        Ok(self
            .client
            .get(&self.url)
            .await?
            .map(|map| map.and_then(|map| map.get(name).cloned())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::std_replacement;
    use docs_rs_types::testing::KRATE;
    use std::time::Duration;

    async fn fixture() -> Result<(mockito::ServerGuard, StdReplacements)> {
        let server = mockito::Server::new_async().await;
        let api = StdReplacements::from_config(
            &Config::builder()
                .url(server.url().parse()?)
                .max_retries(0)
                .build(),
        )?;
        Ok((server, api))
    }

    fn body(description: &str) -> String {
        serde_json::to_string(&ReplacementMap::from_iter([(
            KRATE,
            Arc::new(std_replacement(description)),
        )]))
        .unwrap()
    }

    async fn advance(duration: Duration) {
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
    }

    #[tokio::test]
    async fn crate_lookups_share_dataset_and_preserve_ttl() -> Result<()> {
        let (mut server, api) = fixture().await?;
        let mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body("replacement"))
            .with_header("cache-control", "max-age=600")
            .expect(1)
            .create_async()
            .await;
        let name = KRATE;
        let missing_name = KrateName::from_static("missing");
        let (present, missing) = tokio::try_join!(api.get(&name), api.get(&missing_name))?;
        assert_eq!(present.value.unwrap().description(), "replacement");
        assert!(missing.value.is_none());
        for ttl in [present.ttl, missing.ttl] {
            assert!(ttl <= Duration::from_secs(600));
            assert!(ttl > Duration::from_secs(590));
        }
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn refreshed_dataset_can_remove_replacements() -> Result<()> {
        let (mut server, api) = fixture().await?;
        let initial = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body("old"))
            .with_header("cache-control", "max-age=1")
            .create_async()
            .await;
        api.get(&KRATE).await?;
        initial.remove_async().await;
        let updated = server
            .mock("GET", "/")
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create_async()
            .await;
        advance(Duration::from_secs(2)).await;
        assert!(api.get(&KRATE).await?.value.is_none());
        updated.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn forwards_configured_fallback_ttl() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let api = StdReplacements::from_config(
            &Config::builder()
                .url(server.url().parse()?)
                .max_retries(0)
                .cache_default_ttl(Duration::from_secs(90).into())
                .build(),
        )?;
        let mock = server
            .mock("GET", "/")
            .with_body(body("replacement"))
            .create_async()
            .await;
        let result = api.get(&KRATE).await?;
        assert_eq!(result.value.unwrap().description(), "replacement");
        assert!(result.ttl <= Duration::from_secs(90));
        assert!(result.ttl > Duration::from_secs(85));
        mock.assert_async().await;
        Ok(())
    }
}
