use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};
use masterbus_tools::mapping::{FieldMapping, Mapping};
use serde::{Deserialize, Serialize};

struct AppState {
    mapping_path: PathBuf,
    dump_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mapping_path = std::env::var_os("MAPPING").map(PathBuf::from).map_or_else(
        || masterbus::FileConfig::load_or_create().map(|c| c.mapping_path()),
        Ok,
    )?;

    println!("mapping: {}", mapping_path.display());

    let dump_path = std::env::var_os("MASTERBUS_DUMP").map(PathBuf::from);

    if let Some(path) = &dump_path {
        println!("development dump: {}", path.display());
    }

    let state = Arc::new(AppState {
        mapping_path,
        dump_path,
    });

    let app = Router::new()
        .route("/", get(root))
        .route("/api/mapping", get(get_mapping).put(put_mapping))
        .route("/api/devices", get(get_devices))
        .route("/api/signalk/electrical", get(get_signalk_electrical))
        .route("/api/signalk/compatibility", get(get_signalk_compatibility))
        .route("/api/signalk/candidates", get(get_signalk_candidates))
        .route("/style.css", get(style))
        .route("/app.js", get(app_js))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3008").await?;

    println!("masterbus-web listening on http://0.0.0.0:3008");

    axum::serve(listener, app).await?;

    Ok(())
}

const INDEX_HTML: &str = include_str!("../web/index.html");
const STYLE_CSS: &str = include_str!("../web/style.css");
const APP_JS: &str = include_str!("../web/app.js");

async fn root() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn style() -> impl IntoResponse {
    ([("content-type", "text/css")], STYLE_CSS)
}

async fn app_js() -> impl IntoResponse {
    ([("content-type", "application/javascript")], APP_JS)
}

async fn get_mapping(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match Mapping::load(&state.mapping_path) {
        Ok(mapping) => (StatusCode::OK, Json(mapping)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not load mapping: {e}"),
        )
            .into_response(),
    }
}

async fn put_mapping(
    State(state): State<Arc<AppState>>,
    Json(mapping): Json<Mapping>,
) -> impl IntoResponse {
    match mapping.save(&state.mapping_path) {
        Ok(()) => (StatusCode::OK, Json(mapping)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not save mapping: {e}"),
        )
            .into_response(),
    }
}

async fn get_devices(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(path) = &state.dump_path else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "live MasterBus scanning is not implemented yet",
        )
            .into_response();
    };

    match masterbus_tools::web::load_dump(path) {
        Ok(devices) => (StatusCode::OK, Json(devices)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not load dump: {e}"),
        )
            .into_response(),
    }
}
async fn get_signalk_electrical() -> impl IntoResponse {
    let mut electrical = BTreeMap::new();

    for device_type in masterbus_tools::signalk_schema::electrical_types() {
        electrical.insert(
            device_type.clone(),
            masterbus_tools::signalk_schema::electrical_fields(&device_type),
        );
    }

    (StatusCode::OK, Json(electrical))
}
#[derive(Debug, Deserialize)]
struct CompatibilityQuery {
    path: String,
    unit: String,
}

#[derive(Debug, Deserialize)]
struct CandidatesQuery {
    device_type: String,
    unit: String,
}

#[derive(Debug, Serialize)]
struct Candidate {
    path: String,
    unit: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Serialize)]
struct CompatibilityResponse {
    compatible: bool,
    unit: Option<String>,
    warning: Option<String>,
    refusal: Option<String>,
}

async fn get_signalk_compatibility(Query(query): Query<CompatibilityQuery>) -> impl IntoResponse {
    let entry = FieldMapping {
        path: query.path.clone(),
        invert: false,
        truth: BTreeMap::new(),
    };

    match masterbus_tools::signalk::plan(&query.path, &query.unit, &[], &entry) {
        Ok(plan) => Json(CompatibilityResponse {
            compatible: true,
            unit: plan.unit.map(str::to_owned),
            warning: plan.warning,
            refusal: None,
        }),
        Err(refusal) => Json(CompatibilityResponse {
            compatible: false,
            unit: None,
            warning: None,
            refusal: Some(refusal.to_string()),
        }),
    }
}
async fn get_signalk_candidates(Query(query): Query<CandidatesQuery>) -> impl IntoResponse {
    let mut candidates = Vec::new();

    for field in masterbus_tools::signalk_schema::electrical_fields(&query.device_type) {
        let path = format!("electrical.{}.candidate.{}", query.device_type, field.path);

        if let Some(schema_unit) = &field.unit {
            let Some(source) = masterbus_tools::units::to_si(&query.unit) else {
                continue;
            };

            if source.unit != schema_unit {
                continue;
            }
        } else if masterbus_tools::units::to_si(&query.unit).is_some() {
            continue;
        }

        let entry = FieldMapping {
            path: path.clone(),
            invert: false,
            truth: BTreeMap::new(),
        };

        if let Ok(plan) = masterbus_tools::signalk::plan(&path, &query.unit, &[], &entry) {
            candidates.push(Candidate {
                path: field.path,
                unit: plan.unit.map(str::to_owned),
                description: field.description,
            });
        }
    }

    Json(candidates)
}
