use anyhow::{Context as _, Error, Result, anyhow};
use chrono::prelude::*;
use serde_with::{DeserializeFromStr, SerializeDisplay};
use std::{fmt, str::FromStr};

/// A parsed one-line `rustc --version` string, e.g.
/// `rustc 1.10.0-nightly (57ef01513 2016-05-23)`.
///
/// The original line is kept as-is, since it is what we persist & compare.
/// (De)serialized as the raw version string, compatible with plain `String` values.
#[derive(Clone, PartialEq, Eq, SerializeDisplay, DeserializeFromStr)]
pub struct RustcVersion {
    raw: String,
    release: String,
    commit: String,
    date: NaiveDate,
}

impl RustcVersion {
    /// Build a version from its parts, rendered like real `rustc --version` output.
    pub fn new(release: impl Into<String>, commit: impl Into<String>, date: NaiveDate) -> Self {
        let release = release.into();
        let commit = commit.into();
        Self {
            raw: format!("rustc {release} ({commit} {})", date.format("%Y-%m-%d")),
            release,
            commit,
            date,
        }
    }

    pub fn release(&self) -> &str {
        &self.release
    }

    pub fn commit(&self) -> &str {
        &self.commit
    }

    pub fn date(&self) -> NaiveDate {
        self.date
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Identifier used for the rustdoc `--resource-suffix`,
    /// e.g. `20160523-1.10.0-nightly-57ef01513`.
    pub fn resource_suffix(&self) -> String {
        format!(
            "{}-{}-{}",
            self.date.format("%Y%m%d"),
            self.release,
            self.commit
        )
    }
}

impl FromStr for RustcVersion {
    type Err = Error;

    fn from_str(version: &str) -> Result<Self> {
        let mut outer = version.splitn(3, ' ');
        let _binary = outer.next();
        let release = outer
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("missing release in rustc version `{version}`"))?;
        let details = outer
            .next()
            .and_then(|value| value.strip_prefix('('))
            .and_then(|value| value.strip_suffix(')'))
            .ok_or_else(|| anyhow!("missing details in rustc version `{version}`"))?;
        let mut details = details.split_whitespace();
        let commit = details
            .next()
            .ok_or_else(|| anyhow!("missing commit in rustc version `{version}`"))?;
        let date = details
            .next()
            .ok_or_else(|| anyhow!("missing date in rustc version `{version}`"))?;
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .with_context(|| format!("invalid date in rustc version `{version}`"))?;

        Ok(Self {
            raw: version.to_owned(),
            release: release.to_owned(),
            commit: commit.to_owned(),
            date,
        })
    }
}

impl fmt::Display for RustcVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Only the raw string; the other fields are derived from it.
impl fmt::Debug for RustcVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RustcVersion").field(&self.raw).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(
        "rustc 1.10.0-nightly (57ef01513 2016-05-23)",
        "1.10.0-nightly",
        "57ef01513",
        2016,
        5,
        23
    )]
    #[test_case(
        "rustc 1.80.0 (051478957 2024-07-21)",
        "1.80.0",
        "051478957",
        2024,
        7,
        21
    )]
    #[test_case(
        "rustc 1.81.0-beta.3 (4ac7bcbaa 2024-07-30)",
        "1.81.0-beta.3",
        "4ac7bcbaa",
        2024,
        7,
        30
    )]
    #[test_case("docsrs 0.2.0 (ba9ae23 2016-05-26)", "0.2.0", "ba9ae23", 2016, 5, 26)]
    fn parses(input: &str, release: &str, commit: &str, y: i32, m: u32, d: u32) {
        let version: RustcVersion = input.parse().unwrap();
        assert_eq!(version.release(), release);
        assert_eq!(version.commit(), commit);
        assert_eq!(version.date(), NaiveDate::from_ymd_opt(y, m, d).unwrap());
        assert_eq!(version.as_str(), input);
        assert_eq!(version.to_string(), input);
    }

    #[test]
    fn resource_suffix() {
        let version: RustcVersion = "rustc 1.10.0-nightly (57ef01513 2016-05-23)"
            .parse()
            .unwrap();
        assert_eq!(
            version.resource_suffix(),
            "20160523-1.10.0-nightly-57ef01513"
        );
    }

    #[test]
    fn new_renders_like_rustc() {
        let version = RustcVersion::new(
            "1.9999.0-nightly",
            "0123456789abcdef",
            NaiveDate::from_ymd_opt(2999, 12, 29).unwrap(),
        );
        assert_eq!(
            version.as_str(),
            "rustc 1.9999.0-nightly (0123456789abcdef 2999-12-29)"
        );
        assert_eq!(version.as_str().parse::<RustcVersion>().unwrap(), version);
        assert_eq!(
            version.resource_suffix(),
            "29991229-1.9999.0-nightly-0123456789abcdef"
        );
    }

    #[test]
    fn serde_roundtrips_as_plain_string() {
        let raw = "rustc 1.10.0-nightly (57ef01513 2016-05-23)";
        let version: RustcVersion = raw.parse().unwrap();
        let json = serde_json::to_value(&version).unwrap();
        assert_eq!(json, serde_json::Value::String(raw.into()));
        assert_eq!(
            serde_json::from_value::<RustcVersion>(json).unwrap(),
            version
        );
        assert!(serde_json::from_value::<RustcVersion>("outdated".into()).is_err());
    }

    #[test_case(""; "empty")]
    #[test_case("rustc"; "binary only")]
    #[test_case("rustc 1.80.0-dev"; "dev build without details")]
    #[test_case("rustc 1.80.0 (051478957)"; "missing date")]
    #[test_case("rustc 1.80.0 (051478957 2024-13-40)"; "invalid date")]
    #[test_case("rustc (blabla 2019-01-01)"; "missing release")]
    fn rejects(input: &str) {
        assert!(input.parse::<RustcVersion>().is_err());
    }
}
