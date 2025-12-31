use crate::error::AxumNope;
use crate::page::TemplateData;
use crate::utils::get_correct_docsrs_style_file;
use crate::{
    impl_axum_webpage,
    metrics::WebMetrics,
    page::templates::{RenderBrands, RenderSolid, filters},
};
use anyhow::{Context as _, Error, Result, anyhow, bail};
use askama::Template;
use axum::{
    Router as AxumRouter,
    extract::{Extension, MatchedPath, Request as AxumRequest},
    http::StatusCode,
    middleware,
    middleware::Next,
    response::{IntoResponse, Response as AxumResponse},
};
use axum_extra::middleware::option_layer;
use chrono::{DateTime, NaiveDate, Utc};
use docs_rs_database::crate_details::{Release, parse_doc_targets};
use docs_rs_types::{BuildStatus, CrateId, KrateName, ReqVersion, Version, VersionReq};
use docs_rs_utils::rustc_version::parse_rustc_date;
use sentry::integrations::tower as sentry_tower;
use serde::Serialize;
use std::{
    borrow::Cow,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tower::ServiceBuilder;
use tower_http::{catch_panic::CatchPanicLayer, timeout::TimeoutLayer, trace::TraceLayer};
use tracing::{info, instrument};

/// MetaData used in header
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MetaData {
    pub(crate) name: KrateName,
    /// The exact version of the release being shown.
    pub(crate) version: Version,
    /// The version identifier in the request that was used to request this page.
    /// This might be any of the variants of `ReqVersion`, but
    /// due to a canonicalization step, it is either an Exact version, or `/latest/`
    /// most of the time.
    pub(crate) req_version: ReqVersion,
    pub(crate) description: Option<String>,
    pub(crate) target_name: Option<String>,
    pub(crate) rustdoc_status: Option<bool>,
    pub(crate) default_target: Option<String>,
    pub(crate) doc_targets: Option<Vec<String>>,
    pub(crate) yanked: Option<bool>,
    /// CSS file to use depending on the rustdoc version used to generate this version of this
    /// crate.
    pub(crate) rustdoc_css_file: Option<String>,
}

impl MetaData {
    pub(crate) async fn from_crate(
        conn: &mut sqlx::PgConnection,
        name: &str,
        version: &Version,
        req_version: Option<ReqVersion>,
    ) -> Result<MetaData> {
        let row = sqlx::query!(
            r#"SELECT
                crates.name as "name: KrateName",
                releases.version,
                releases.description,
                releases.target_name,
                releases.rustdoc_status,
                releases.default_target,
                releases.doc_targets,
                releases.yanked,
                builds.rustc_version as "rustc_version?"
            FROM releases
            INNER JOIN crates ON crates.id = releases.crate_id
            LEFT JOIN LATERAL (
                SELECT * FROM builds
                WHERE builds.rid = releases.id
                ORDER BY builds.build_finished
                DESC LIMIT 1
            ) AS builds ON true
            WHERE crates.name = $1 AND releases.version = $2"#,
            name,
            version.to_string(),
        )
        .fetch_one(&mut *conn)
        .await
        .context("error fetching crate metadata")?;

        Ok(MetaData {
            name: row.name,
            version: version.clone(),
            req_version: req_version.unwrap_or_else(|| ReqVersion::Exact(version.clone())),
            description: row.description,
            target_name: row.target_name,
            rustdoc_status: row.rustdoc_status,
            default_target: row.default_target,
            doc_targets: row.doc_targets.map(parse_doc_targets),
            yanked: row.yanked,
            rustdoc_css_file: row
                .rustc_version
                .as_deref()
                .map(get_correct_docsrs_style_file)
                .transpose()?,
        })
    }
}
