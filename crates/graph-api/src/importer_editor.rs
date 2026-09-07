//! Importer package definition editing.
//!
//! `GET /importers/:id/definition` exposes an httpjson source's authored
//! package TOML with the catalog's read posture (none).
//! `PUT /importers/:id/definition` and `POST /importers` mutate the packages
//! directory and therefore carry the runtime-switching authorization: the
//! caller's groups header must contain the configured switch group, exactly
//! as selecting a non-default source does. Every write validates first, then
//! lands atomically. Runtime-added sources persist in
//! `<packages_dir>/catalog.local.json`, merged into the chart catalog at boot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::body::Bytes;
use axum::extract::{Path as RoutePath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::importer_catalog::{
    self, CatalogSourceKind, ImporterCatalogOverlay, ImporterHttpJsonSource,
    ImporterSourceDefinition, OVERLAY_CATALOG_FILENAME,
};
use crate::importer_package::{
    dir_is_writable, parse_importer_package, read_package_definition, require_toml,
    write_atomically,
};
use crate::source_host::SourceHost;

/// `GET`/`PUT /importers/:id/definition` body.
#[derive(Debug, Serialize)]
pub struct DefinitionResponse {
    pub package: String,
    pub source: String,
    pub writable: bool,
}

#[derive(Deserialize)]
pub struct DefinitionPutReq {
    source: String,
}

/// `POST /importers` body.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateImporterReq {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    package: String,
    source: String,
    endpoint: String,
    #[serde(default)]
    variables: BTreeMap<String, String>,
    #[serde(default)]
    poll_interval_ms: Option<u64>,
}

fn reject(status: StatusCode, message: impl Into<String>) -> Response {
    (status, message.into()).into_response()
}

fn authorize(host: &SourceHost, headers: &HeaderMap) -> Result<(), Response> {
    if host.switch().authorize(headers) {
        Ok(())
    } else {
        Err(reject(
            StatusCode::FORBIDDEN,
            "editing importer definitions requires runtime switching to be enabled \
             and the configured group",
        ))
    }
}

/// Mutation bodies are parsed only after authorization so an unauthorized
/// caller learns nothing about the accepted shape.
fn parse_body<T: DeserializeOwned>(body: &Bytes) -> Result<T, Response> {
    serde_json::from_slice(body)
        .map_err(|error| reject(StatusCode::BAD_REQUEST, format!("invalid JSON body: {error}")))
}

struct Located {
    packages_dir: PathBuf,
    package: String,
    path: PathBuf,
}

/// Resolve a catalog source id to its package file.
fn locate(host: &SourceHost, source_id: &str) -> Result<Located, Response> {
    let catalog = host.catalog();
    let Some(definition) = catalog.source(source_id) else {
        return Err(reject(
            StatusCode::NOT_FOUND,
            format!("unknown importer source {source_id:?}"),
        ));
    };
    let Some(http_json) = &definition.http_json else {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            format!("importer source {source_id:?} is not an httpjson package source"),
        ));
    };
    let packages_dir = packages_dir(host)?;
    Ok(Located {
        path: packages_dir.join(&http_json.package),
        package: http_json.package.clone(),
        packages_dir,
    })
}

fn packages_dir(host: &SourceHost) -> Result<PathBuf, Response> {
    host.packages_dir().map(Path::to_path_buf).ok_or_else(|| {
        reject(
            StatusCode::SERVICE_UNAVAILABLE,
            "JUMP_CANNON_IMPORTER_PACKAGES_DIR is not configured; package definitions are unavailable",
        )
    })
}

/// Validate authored package text off the async runtime: validation compiles
/// the declared grammar and is synchronous CPU work. A validation failure is
/// the 400 body verbatim.
async fn validate_source_text(source: String) -> Result<(), Response> {
    match tokio::task::spawn_blocking(move || parse_importer_package(&source)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(reject(StatusCode::BAD_REQUEST, error)),
        Err(error) => Err(reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("package validation task failed: {error}"),
        )),
    }
}

fn require_writable(dir: &Path) -> Result<(), Response> {
    if dir_is_writable(dir) {
        Ok(())
    } else {
        Err(reject(
            StatusCode::CONFLICT,
            format!(
                "packages directory {} is read-only; importer definitions cannot be written",
                dir.display()
            ),
        ))
    }
}

pub async fn definition_get(
    State(host): State<SourceHost>,
    RoutePath(source_id): RoutePath<String>,
) -> Response {
    let located = match locate(&host, &source_id) {
        Ok(located) => located,
        Err(response) => return response,
    };
    if !located.path.is_file() {
        return reject(
            StatusCode::NOT_FOUND,
            format!(
                "package file {:?} for importer source {source_id:?} is not present in the packages directory",
                located.package
            ),
        );
    }
    let definition = match read_package_definition(&located.path) {
        Ok(definition) => definition,
        Err(error) => return reject(StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    Json(DefinitionResponse {
        package: located.package,
        source: definition.source,
        writable: dir_is_writable(&located.packages_dir),
    })
    .into_response()
}

pub async fn definition_put(
    State(host): State<SourceHost>,
    headers: HeaderMap,
    RoutePath(source_id): RoutePath<String>,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&host, &headers) {
        return response;
    }
    let req: DefinitionPutReq = match parse_body(&body) {
        Ok(req) => req,
        Err(response) => return response,
    };
    let located = match locate(&host, &source_id) {
        Ok(located) => located,
        Err(response) => return response,
    };
    if let Err(error) = require_toml(&located.path) {
        return reject(StatusCode::BAD_REQUEST, error);
    }
    if let Err(response) = validate_source_text(req.source.clone()).await {
        return response;
    }
    if let Err(response) = require_writable(&located.packages_dir) {
        return response;
    }
    if let Err(error) = write_atomically(&located.path, &req.source) {
        return reject(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    // The next selection rebuilds from the new file. The deployment default's
    // own importer is bound at boot and keeps its loaded package.
    host.invalidate_alternate(&source_id);
    tracing::info!(source = %source_id, package = %located.package, "importer package definition updated");
    Json(DefinitionResponse {
        package: located.package,
        source: req.source,
        writable: true,
    })
    .into_response()
}

/// Package filenames are joined onto the packages directory, so the charset
/// is the stable-id charset, no leading dot (temp/probe files live there),
/// and a `.toml` extension.
fn validate_package_filename(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'));
    if !valid {
        return Err(
            "package must be a lowercase filename of letters, digits, '-', '_' or '.' \
             ending in .toml"
                .into(),
        );
    }
    require_toml(Path::new(name))
}

pub async fn importers_post(
    State(host): State<SourceHost>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&host, &headers) {
        return response;
    }
    let req: CreateImporterReq = match parse_body(&body) {
        Ok(req) => req,
        Err(response) => return response,
    };
    if let Err(error) = validate_package_filename(&req.package) {
        return reject(StatusCode::BAD_REQUEST, error);
    }
    let definition = ImporterSourceDefinition {
        display_name: req.name,
        description: req.description,
        kind: CatalogSourceKind::HttpJson,
        source_id: None,
        filesystem_rescan_interval_seconds: None,
        source: None,
        http_json: Some(ImporterHttpJsonSource {
            package: req.package.clone(),
            endpoint: req.endpoint,
            variables: req.variables,
            token_env: None,
            poll_interval_ms: req.poll_interval_ms.unwrap_or(60_000),
        }),
        producer: None,
    };
    if let Err(error) = importer_catalog::validate_definition(&req.id, &definition) {
        return reject(StatusCode::BAD_REQUEST, error);
    }
    if host.catalog().source(&req.id).is_some() {
        return reject(
            StatusCode::CONFLICT,
            format!("importer source {:?} already exists", req.id),
        );
    }
    let packages_dir = match packages_dir(&host) {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let package_path = packages_dir.join(&req.package);
    if package_path.exists() {
        return reject(
            StatusCode::CONFLICT,
            format!("package file {:?} already exists", req.package),
        );
    }
    if let Err(response) = validate_source_text(req.source.clone()).await {
        return response;
    }
    if let Err(response) = require_writable(&packages_dir) {
        return response;
    }

    // Persist first, then publish: the served catalog only ever reflects
    // what boot would reload from disk.
    if let Err(error) = write_atomically(&package_path, &req.source) {
        return reject(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    if let Err(error) = append_overlay(&packages_dir, &req.id, &definition) {
        let _ = std::fs::remove_file(&package_path);
        return reject(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    if let Err(error) = host.add_runtime_source(req.id.clone(), definition) {
        return reject(StatusCode::CONFLICT, error);
    }
    tracing::info!(source = %req.id, package = %req.package, "importer source added at runtime");
    match host.catalog().item(&req.id) {
        Some(item) => (StatusCode::CREATED, Json(item)).into_response(),
        None => reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime source vanished after insertion",
        ),
    }
}

/// Read-modify-write `catalog.local.json`. A pre-existing overlay that no
/// longer parses is left untouched and the add fails loudly.
fn append_overlay(
    packages_dir: &Path,
    id: &str,
    definition: &ImporterSourceDefinition,
) -> Result<(), String> {
    let path = packages_dir.join(OVERLAY_CATALOG_FILENAME);
    let mut overlay = match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str::<ImporterCatalogOverlay>(&raw)
            .map_err(|error| format!("{} is not a valid overlay catalog: {error}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ImporterCatalogOverlay::default()
        }
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    overlay.sources.insert(id.to_owned(), definition.clone());
    let text = serde_json::to_string_pretty(&overlay)
        .map_err(|error| format!("failed to encode overlay catalog: {error}"))?;
    write_atomically(&path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_filenames_are_bounded_to_the_packages_dir() {
        assert!(validate_package_filename("a-b_c.1.toml").is_ok());
        assert!(validate_package_filename("pkg.toml").is_ok());
        for bad in ["", ".hidden.toml", "../x.toml", "a/b.toml", "Upper.toml", "x.yaml", "..", "pkg.nix"] {
            assert!(validate_package_filename(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
