//! Shipped example sessions: curated app-state + source pairs that show one
//! simulation/exploration regime each.
//!
//! The files ride the frontend dist (`assets/sessions/`), not the server, so
//! they work in every deployment — the container, `trunk serve`, and the
//! browser-only GitHub Pages mode — without a chart mount or a dev-only
//! `configs/` directory.
//!
//! Loading one is two steps: apply the persisted UI state exactly like an
//! imported YAML (`appstate::import_str` + `appstate::apply`), then, when the
//! session names an importer source, switch this browser session's view to it
//! through the Importers panel's apply path — which starts the source's
//! background build and streams its progress if the server has not built it
//! yet.

use dioxus::prelude::*;

use crate::{api, appstate, Ctx};

/// One entry of `assets/sessions/index.json`.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(crate) struct ExampleSession {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) description: String,
    /// Catalog id whose graph the session explores; `None` runs on the
    /// deployment default source.
    #[serde(default)]
    pub(crate) source: Option<String>,
}

/// Fetch state of the shipped index.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Catalog {
    Idle,
    Loading,
    Ready(Vec<ExampleSession>),
    Unavailable(String),
}

pub(crate) static CATALOG: GlobalSignal<Catalog> = Signal::global(|| Catalog::Idle);
/// Name of the session currently being applied, for the button's busy state.
pub(crate) static APPLYING: GlobalSignal<Option<String>> = Signal::global(|| None);
pub(crate) static ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);

/// Dist-relative URL of a shipped session asset.
fn asset_url(path: &str) -> String {
    // Same-origin asset of the served bundle; the server URL setting points at
    // graph-api, which is not necessarily where the dist came from.
    format!("assets/sessions/{path}")
}

/// Kick the one-shot index fetch (idempotent).
pub(crate) fn ensure_catalog() {
    if !matches!(*CATALOG.peek(), Catalog::Idle) {
        return;
    }
    *CATALOG.write() = Catalog::Loading;
    spawn(async move {
        *CATALOG.write() = match fetch_index().await {
            Ok(sessions) => Catalog::Ready(sessions),
            Err(error) => Catalog::Unavailable(error),
        };
    });
}

async fn fetch_index() -> Result<Vec<ExampleSession>, String> {
    let response = gloo_net::http::Request::get(&asset_url("index.json"))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.ok() {
        return Err(format!("index.json -> HTTP {}", response.status()));
    }
    response.json().await.map_err(|error| error.to_string())
}

/// Apply one shipped session: UI state first, then the source switch.
pub(crate) fn load(ctx: Ctx, session: ExampleSession) {
    if APPLYING.peek().is_some() {
        return;
    }
    *APPLYING.write() = Some(session.name.clone());
    *ERROR.write() = None;
    spawn(async move {
        let result = async {
            let response = gloo_net::http::Request::get(&asset_url(&format!(
                "{}.yaml",
                session.name
            )))
            .send()
            .await
            .map_err(|error| error.to_string())?;
            if !response.ok() {
                return Err(format!("{}.yaml -> HTTP {}", session.name, response.status()));
            }
            let yaml = response.text().await.map_err(|error| error.to_string())?;
            let state = appstate::import_str(&yaml).map_err(|error| error.to_string())?;
            appstate::apply(&state, "example session");
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                // The source switch runs through the Importers panel's apply
                // path so a not-yet-built source gets the same background
                // build, progress overlay, and retry loop as a manual load.
                match &session.source {
                    Some(id)
                        if api::source_selection().map(|s| s.id).as_deref()
                            != Some(id.as_str()) =>
                    {
                        crate::panels::importers::apply_catalog_source(ctx, id.clone());
                    }
                    Some(_) => {}
                    None if api::source_selection().is_some() => {
                        crate::panels::importers::return_to_default_source(ctx);
                    }
                    None => {}
                }
            }
            Err(error) => *ERROR.write() = Some(error),
        }
        APPLYING.write().take();
    });
}
