use crate::error::AxumNope;
use anyhow::{Context as _, Result};
use docs_rs_database::crate_details::Release;
use docs_rs_types::{BuildStatus, CrateId, KrateName, ReqVersion, Version, VersionReq};
use tracing::instrument;

#[derive(Debug)]
pub(crate) struct MatchedRelease {
    /// crate name
    pub name: KrateName,

    /// The crate name that was found when attempting to load a crate release.
    /// `match_version` will attempt to match a provided crate name against similar crate names with
    /// dashes (`-`) replaced with underscores (`_`) and vice versa.
    pub corrected_name: Option<KrateName>,

    /// what kind of version did we get in the request? ("latest", semver, exact)
    pub req_version: ReqVersion,

    /// the matched release
    pub release: Release,

    /// all releases since we have them anyways and so we can pass them to CrateDetails
    pub(crate) all_releases: Vec<Release>,
}

impl MatchedRelease {
    pub(crate) fn assume_exact_name(self) -> Result<Self, AxumNope> {
        if self.corrected_name.is_none() {
            Ok(self)
        } else {
            Err(AxumNope::CrateNotFound)
        }
    }

    pub(crate) fn into_exactly_named(self) -> Self {
        if let Some(corrected_name) = self.corrected_name {
            Self {
                name: corrected_name.to_owned(),
                corrected_name: None,
                ..self
            }
        } else {
            self
        }
    }

    pub(crate) fn into_exactly_named_or_else<F>(self, f: F) -> Result<Self, AxumNope>
    where
        F: FnOnce(&KrateName, &ReqVersion) -> AxumNope,
    {
        if let Some(corrected_name) = self.corrected_name {
            Err(f(&corrected_name, &self.req_version))
        } else {
            Ok(self)
        }
    }

    /// Canonicalize the version from the request
    ///
    /// Mainly:
    /// * "newest"/"*" or empty -> "latest" in the URL
    /// * any other semver requirement -> specific version in the URL
    pub(crate) fn into_canonical_req_version(self) -> Self {
        match self.req_version {
            ReqVersion::Exact(_) | ReqVersion::Latest => self,
            ReqVersion::Semver(version_req) => {
                if version_req == VersionReq::STAR {
                    Self {
                        req_version: ReqVersion::Latest,
                        ..self
                    }
                } else {
                    Self {
                        req_version: ReqVersion::Exact(self.release.version.clone()),
                        ..self
                    }
                }
            }
        }
    }

    /// translate this MatchRelease into a specific semver::Version while canonicalizing the
    /// version specification.
    pub(crate) fn into_canonical_req_version_or_else<F>(self, f: F) -> Result<Self, AxumNope>
    where
        F: FnOnce(&KrateName, &ReqVersion) -> AxumNope,
    {
        let original_req_version = self.req_version.clone();
        let canonicalized = self.into_canonical_req_version();

        if canonicalized.req_version == original_req_version {
            Ok(canonicalized)
        } else {
            Err(f(&canonicalized.name, &canonicalized.req_version))
        }
    }

    pub(crate) fn into_version(self) -> Version {
        self.release.version
    }

    pub(crate) fn build_status(&self) -> BuildStatus {
        self.release.build_status
    }

    pub(crate) fn rustdoc_status(&self) -> bool {
        self.release.rustdoc_status.unwrap_or(false)
    }

    pub(crate) fn is_latest_url(&self) -> bool {
        matches!(self.req_version, ReqVersion::Latest)
    }
}

fn semver_match<'a, F: Fn(&Release) -> bool>(
    releases: &'a [Release],
    req: &VersionReq,
    filter: F,
) -> Option<&'a Release> {
    // first try standard semver match using `VersionReq::match`, should handle most cases.
    if let Some(release) = releases
        .iter()
        .filter(|release| filter(release))
        .find(|release| req.matches(&release.version))
    {
        Some(release)
    } else if req == &VersionReq::STAR {
        // semver `*` does not match pre-releases.
        // So when we only have pre-releases, `VersionReq::STAR` would lead to an
        // empty result.
        // In this case we just return the latest prerelease instead of nothing.
        releases.iter().find(|release| filter(release))
    } else {
        None
    }
}

/// Checks the database for crate releases that match the given name and version.
///
/// `version` may be an exact version number or loose semver version requirement. The return value
/// will indicate whether the given version exactly matched a version number from the database.
///
/// This function will also check for crates where dashes in the name (`-`) have been replaced with
/// underscores (`_`) and vice-versa. The return value will indicate whether the crate name has
/// been matched exactly, or if there has been a "correction" in the name that matched instead.
#[instrument(skip(conn))]
pub(crate) async fn match_version(
    conn: &mut sqlx::PgConnection,
    name: &str,
    input_version: &ReqVersion,
) -> Result<MatchedRelease, AxumNope> {
    let (crate_id, name, corrected_name) = {
        let row = sqlx::query!(
            r#"
             SELECT
                id as "id: CrateId",
                name as "name: KrateName"
             FROM crates
             WHERE normalize_crate_name(name) = normalize_crate_name($1)"#,
            name,
        )
        .fetch_optional(&mut *conn)
        .await
        .context("error fetching crate")?
        .ok_or(AxumNope::CrateNotFound)?;

        let name: KrateName = name
            .parse()
            .expect("here we know it's valid, because we found it after normalizing");

        if row.name != name {
            (row.id, name, Some(row.name))
        } else {
            (row.id, name, None)
        }
    };

    // first load and parse all versions of this crate,
    // `releases_for_crate` is already sorted, newest version first.
    let releases = docs_rs_database::crate_details::releases_for_crate(conn, crate_id)
        .await
        .context("error fetching releases for crate")?;

    if releases.is_empty() {
        return Err(AxumNope::CrateNotFound);
    }

    let req_semver: VersionReq = match input_version {
        ReqVersion::Exact(parsed_req_version) => {
            if let Some(release) = releases
                .iter()
                .find(|release| &release.version == parsed_req_version)
            {
                return Ok(MatchedRelease {
                    name,
                    corrected_name,
                    req_version: input_version.clone(),
                    release: release.clone(),
                    all_releases: releases,
                });
            }

            if let Ok(version_req) = VersionReq::parse(&parsed_req_version.to_string()) {
                // when we don't find a release with exact version,
                // we try to interpret it as a semver requirement.
                // A normal semver version ("1.2.3") is equivalent to a caret semver requirement.
                version_req
            } else {
                return Err(AxumNope::VersionNotFound);
            }
        }
        ReqVersion::Latest => VersionReq::STAR,
        ReqVersion::Semver(version_req) => version_req.clone(),
    };

    // when matching semver requirements,
    // we generally only want to look at non-yanked releases,
    // excluding releases which just contain in-progress builds
    if let Some(release) = semver_match(&releases, &req_semver, |r: &Release| {
        r.build_status != BuildStatus::InProgress && (r.yanked.is_none() || r.yanked == Some(false))
    }) {
        return Ok(MatchedRelease {
            name: name.to_owned(),
            corrected_name,
            req_version: input_version.clone(),
            release: release.clone(),
            all_releases: releases,
        });
    }

    // when we don't find any match with "normal" releases, we also look into in-progress releases
    if let Some(release) = semver_match(&releases, &req_semver, |r: &Release| {
        r.yanked.is_none() || r.yanked == Some(false)
    }) {
        return Ok(MatchedRelease {
            name: name.to_owned(),
            corrected_name,
            req_version: input_version.clone(),
            release: release.clone(),
            all_releases: releases,
        });
    }

    // Since we return with a CrateNotFound earlier if the db reply is empty,
    // we know that versions were returned but none satisfied the version requirement.
    // This can only happen when all versions are yanked.
    Err(AxumNope::VersionNotFound)
}
