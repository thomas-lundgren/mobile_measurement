//! Coverage map and charts: `GET /viz` and `GET /viz/charts` serve static
//! pages (no data in them); the pages log in as an operator and fetch the
//! data endpoints below. The data files are written by the analysis package
//! (`python -m fieldtest_analysis.track` / `.series`) under
//! `<upload_root>/viz/`; server-main only serves them. Positions and
//! timelines reveal where the device has been, so the data endpoints require
//! an operator JWT.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;

use crate::auth::authed_operator;
use crate::error::AppError;
use crate::AppState;

const MAP_PAGE: &str = include_str!("viz.html");
const CHARTS_PAGE: &str = include_str!("viz_charts.html");
const FAVICON: &[u8] = include_bytes!("favicon.png");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/viz", get(map_page))
        .route("/viz/charts", get(charts_page))
        .route("/favicon.png", get(favicon))
        .route("/favicon.ico", get(favicon))
        .route("/api/v1/viz/track", get(track))
        .route("/api/v1/viz/series", get(series_days))
        .route("/api/v1/viz/series/{day}", get(series_day))
}

async fn map_page() -> Html<&'static str> {
    Html(MAP_PAGE)
}

async fn charts_page() -> Html<&'static str> {
    Html(CHARTS_PAGE)
}

async fn favicon() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        FAVICON,
    )
}

async fn track(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    serve_json(&st, &headers, PathBuf::from("track.json")).await
}

async fn series_days(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    serve_json(&st, &headers, PathBuf::from("series/days.json")).await
}

async fn series_day(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(day): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // YYYY-MM-DD only: the day becomes a file name.
    let valid = day.len() == 10
        && day.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if !valid {
        return Err(AppError::BadRequest("day must be YYYY-MM-DD".into()));
    }
    serve_json(&st, &headers, PathBuf::from(format!("series/{day}.json"))).await
}

/// Operator-only: return `<upload_root>/viz/<rel>` as JSON.
async fn serve_json(
    st: &AppState,
    headers: &HeaderMap,
    rel: PathBuf,
) -> Result<impl IntoResponse, AppError> {
    authed_operator(st, headers)?;
    let path = st.upload_root.join("viz").join(rel);
    let body = tokio::fs::read(&path).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => AppError::NotFound,
        _ => AppError::Internal,
    })?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    ))
}
