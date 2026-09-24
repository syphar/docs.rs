//! HTTP access to RustSec's per-package OSV advisory feeds.
//!
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use docs_rs_rustsec::{Config, RustsecClient};
//!
//! let client = RustsecClient::from_config(&Config::builder().build())?;
//! let advisory = client.find_unmaintained(&"owned-alloc".parse()?).await?;
//! # Ok(())
//! # }
//! ```
mod config;
mod models;

pub use config::{Config, ConfigBuilder};
pub use models::advisory::{Id, Informational};
pub use models::osv::{OsvAdvisory, OsvAffected, OsvJsonRange, OsvTimelineEvent};

use anyhow::{Context, Result, ensure};
use docs_rs_reqwest::{CachedResult, Client};
use docs_rs_types::KrateName;
use std::sync::Arc;
use tracing::instrument;
use url::Url;

/// A reusable HTTP client caching parsed results, shared by clones.
/// Missing feeds are cached as empty lists using response freshness headers,
/// with the configured TTL as fallback.
#[derive(Debug, Clone)]
pub struct RustsecClient {
    client: Client<Vec<Arc<OsvAdvisory>>>,
    base_url: Url,
}

impl RustsecClient {
    /// Fetch the first unmaintained advisory that has not been withdrawn and
    /// does not offer a patched version in any affected entry, matching crates.io.
    ///
    /// Returns `None` when there is no matching advisory. Fetch errors propagate
    /// to the caller.
    #[instrument(skip(self), fields(krate = %name))]
    pub async fn find_unmaintained(
        &self,
        name: &KrateName,
    ) -> Result<CachedResult<Option<Arc<OsvAdvisory>>>> {
        Ok(self.fetch_advisories(name).await?.map(|advisories| {
            advisories
                .iter()
                .find(|advisory| {
                    !advisory.withdrawn()
                        && advisory.affected().iter().any(|entry| {
                            entry.informational() == Some(&Informational::Unmaintained)
                        })
                        && !advisory
                            .affected()
                            .iter()
                            .any(OsvAffected::has_patched_versions)
                })
                .cloned()
        }))
    }

    /// Construct a client without making a request.
    pub fn from_config(config: &Config) -> Result<Self> {
        ensure!(
            matches!(config.base_url.scheme(), "http" | "https")
                && config.base_url.host_str().is_some()
                && config.base_url.query().is_none()
                && config.base_url.fragment().is_none(),
            "RustSec base URL must be an HTTP(S) URL without a query or fragment"
        );
        Ok(Self {
            client: Client::builder()
                .max_retries(config.max_retries)
                .cache_capacity(config.cache_capacity)
                .default_ttl(config.cache_default_ttl)
                .not_found_ttl(config.cache_default_ttl)
                .build()?,
            base_url: config.base_url.clone(),
        })
    }

    /// Fetch all advisories, including informational and withdrawn records.
    /// Missing feeds become empty lists. Other HTTP failures and malformed JSON
    /// return an error.
    #[instrument(skip(self), fields(krate = %name))]
    pub async fn fetch_advisories(
        &self,
        name: &KrateName,
    ) -> Result<CachedResult<Arc<Vec<Arc<OsvAdvisory>>>>> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .expect("HTTP(S) base URL validated during construction")
            .pop_if_empty()
            .extend(["packages", &format!("{name}.json")]);

        Ok(self
            .client
            .get(&url)
            .await
            .with_context(|| format!("failed to fetch RustSec advisories for {name}"))?
            .map(Option::unwrap_or_default))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::time::Duration;
    use test_case::test_case;

    const OWNED_ALLOC_ADVISORIES: &str = include_str!("../tests/fixtures/owned-alloc.json");
    const OWNED_ALLOC: KrateName = KrateName::from_static("owned-alloc");
    const PATH: &str = "/packages/owned-alloc.json";

    fn client(server: &mockito::Server, max_retries: u32) -> Result<RustsecClient> {
        RustsecClient::from_config(
            &Config::builder()
                .base_url(server.url().parse()?)
                .max_retries(max_retries)
                .build(),
        )
    }

    #[tokio::test]
    async fn missing_feed_is_cached_as_empty_with_configured_ttl() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let missing = server
            .mock("GET", PATH)
            .with_status(404)
            .with_body("HTML error page")
            .expect(1)
            .create_async()
            .await;
        let api = RustsecClient::from_config(
            &Config::builder()
                .base_url(server.url().parse()?)
                .max_retries(0)
                .cache_default_ttl(Duration::from_secs(90).into())
                .build(),
        )?;
        let advisories = api.fetch_advisories(&OWNED_ALLOC).await?;
        assert!(advisories.value.is_empty());
        assert!(advisories.ttl <= Duration::from_secs(90));
        assert!(advisories.ttl > Duration::from_secs(85));
        let warning = api.find_unmaintained(&OWNED_ALLOC).await?;
        assert!(warning.value.is_none());
        assert!(warning.ttl <= advisories.ttl);
        assert!(warning.ttl > Duration::from_secs(85));
        missing.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn finds_unmaintained_advisory() -> Result<()> {
        let advisories = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        assert_unmaintained(advisories, Some("RUSTSEC-2026-0299")).await
    }

    #[tokio::test]
    async fn ignores_withdrawn_unmaintained_advisory() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        advisories[0]["withdrawn"] = json!("2026-09-23T12:00:00Z");
        assert_unmaintained(advisories, None).await
    }

    #[tokio::test]
    async fn ignores_unmaintained_advisory_with_patched_version() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        advisories[0]["affected"][0]["ranges"][0]["events"] =
            json!([{ "introduced": "0" }, { "fixed": "1.0.0" }]);
        assert_unmaintained(advisories, None).await
    }

    #[tokio::test]
    async fn ignores_unmaintained_advisory_with_patched_version_in_other_entry() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        let mut entry = advisories[0]["affected"][0].clone();
        entry["database_specific"]["informational"] = Value::Null;
        entry["ranges"] = json!([{
            "type": "SEMVER", "events": [{ "fixed": "1.0.0" }]
        }]);
        advisories[0]["affected"]
            .as_array_mut()
            .unwrap()
            .push(entry);
        assert_unmaintained(advisories, None).await
    }

    #[tokio::test]
    async fn ignores_security_advisory() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        advisories.as_array_mut().unwrap().remove(0);
        assert_unmaintained(advisories, None).await
    }

    #[tokio::test]
    async fn ignores_other_informational_advisory() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        advisories[0]["affected"][0]["database_specific"]["informational"] = json!("unsound");
        assert_unmaintained(advisories, None).await
    }

    #[tokio::test]
    async fn ignores_advisory_without_affected_entries() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        advisories[0]["affected"] = json!([]);
        assert_unmaintained(advisories, None).await
    }

    #[tokio::test]
    async fn finds_unmaintained_advisory_without_ranges() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        advisories[0]["affected"][0]
            .as_object_mut()
            .unwrap()
            .remove("ranges");
        assert_unmaintained(advisories, Some("RUSTSEC-2026-0299")).await
    }

    #[tokio::test]
    async fn finds_no_unmaintained_advisory_in_empty_feed() -> Result<()> {
        assert_unmaintained(json!([]), None).await
    }

    #[tokio::test]
    async fn finds_first_unmaintained_advisory() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        let mut second = advisories[0].clone();
        second["id"] = json!("RUSTSEC-2026-0300");
        advisories.as_array_mut().unwrap().push(second);
        assert_unmaintained(advisories, Some("RUSTSEC-2026-0299")).await
    }

    #[tokio::test]
    async fn finds_unmaintained_advisory_after_withdrawn_advisory() -> Result<()> {
        let mut advisories: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        let mut second = advisories[0].clone();
        second["id"] = json!("RUSTSEC-2026-0300");
        advisories.as_array_mut().unwrap().push(second);
        advisories[0]["withdrawn"] = json!("2026-09-23T12:00:00Z");
        assert_unmaintained(advisories, Some("RUSTSEC-2026-0300")).await
    }

    async fn assert_unmaintained(advisories: Value, expected: Option<&str>) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", PATH)
            .with_status(200)
            .with_body(advisories.to_string())
            .create_async()
            .await;
        let result = client(&server, 0)?
            .find_unmaintained(&OWNED_ALLOC)
            .await?
            .value;
        assert_eq!(
            result.as_ref().map(|advisory| advisory.id().as_str()),
            expected
        );
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn fetches_owned_alloc_advisories_with_rustsec_metadata() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(OWNED_ALLOC_ADVISORIES)
            .create_async()
            .await;
        let advisories = client(&server, 0)?
            .fetch_advisories(&OWNED_ALLOC)
            .await?
            .value;
        assert_eq!(advisories.len(), 2);

        let unmaintained = &advisories[0];
        assert_eq!(unmaintained.id().as_str(), "RUSTSEC-2026-0299");
        assert_eq!(unmaintained.summary(), "`owned-alloc` is unmaintained");
        assert_eq!(
            unmaintained.affected()[0].informational(),
            Some(&Informational::Unmaintained)
        );

        let security = &advisories[1];
        assert_eq!(security.id().as_str(), "RUSTSEC-2026-0291");
        assert_eq!(security.affected()[0].informational(), None);
        mock.assert_async().await;
        Ok(())
    }

    #[test_case(200, "[]"; "empty feed")]
    #[tokio::test]
    async fn no_advisories_returns_empty(status: usize, body: &str) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/packages/serde.json")
            .with_status(status)
            .with_body(body)
            .create_async()
            .await;
        assert!(
            client(&server, 0)?
                .fetch_advisories(&"serde".parse()?)
                .await?
                .value
                .is_empty()
        );
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn fetch_error_includes_crate_name() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", PATH)
            .with_status(500)
            .with_body("server error")
            .create_async()
            .await;
        let error = client(&server, 0)?
            .fetch_advisories(&OWNED_ALLOC)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(OWNED_ALLOC.as_str()));
        assert_eq!(
            error
                .chain()
                .find_map(|error| error.downcast_ref::<reqwest::Error>())
                .unwrap()
                .status()
                .unwrap()
                .as_u16(),
            500
        );
        mock.assert_async().await;
        Ok(())
    }

    #[test_case("{}"; "wrong top-level shape")]
    #[test_case("[{}]"; "incomplete advisory")]
    #[tokio::test]
    async fn invalid_response_is_an_error(body: &str) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", PATH)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let error = client(&server, 0)?
            .fetch_advisories(&OWNED_ALLOC)
            .await
            .unwrap_err();
        assert!(
            error
                .chain()
                .find_map(|error| error.downcast_ref::<reqwest::Error>())
                .unwrap()
                .is_decode()
        );
        mock.assert_async().await;
        Ok(())
    }

    #[test_case("/mirror"; "without trailing slash")]
    #[test_case("/mirror/"; "with trailing slash")]
    #[tokio::test]
    async fn preserves_base_path_and_crate_name(base_path: &str) -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/mirror/packages/lazy_static.json")
            .with_status(200)
            .with_body("[]")
            .expect(1)
            .create_async()
            .await;
        let config = Config::builder()
            .base_url(format!("{}{base_path}", server.url()).parse()?)
            .build();
        let api = RustsecClient::from_config(&config)?;
        assert!(
            api.fetch_advisories(&"lazy_static".parse()?)
                .await?
                .value
                .is_empty()
        );
        mock.assert_async().await;
        Ok(())
    }

    #[test_case("file:///tmp/rustsec"; "non HTTP URL")]
    #[test_case("https://rustsec.org/?query=value"; "query")]
    #[test_case("https://rustsec.org/#fragment"; "fragment")]
    fn invalid_base_url_is_rejected(url: &str) -> Result<()> {
        let config = Config::builder().base_url(url.parse()?).build();
        assert!(RustsecClient::from_config(&config).is_err());
        Ok(())
    }

    #[test]
    fn parses_withdrawn_and_unknown_informational_kind() -> Result<()> {
        let mut source: Value = serde_json::from_str(OWNED_ALLOC_ADVISORIES)?;
        let value = &mut source[0];
        value["withdrawn"] = json!("2026-09-23T12:00:00Z");
        value["affected"][0]["database_specific"]["informational"] = json!("future-notice");
        let advisory: OsvAdvisory = serde_json::from_value(value.clone())?;
        assert!(advisory.withdrawn());
        assert_eq!(
            advisory.affected()[0].informational(),
            Some(&Informational::Other("future-notice".into()))
        );
        Ok(())
    }
}
