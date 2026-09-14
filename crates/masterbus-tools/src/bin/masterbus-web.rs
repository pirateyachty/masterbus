use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};
use masterbus_tools::mapping::Mapping;

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
        .route("/api/mapping", get(get_mapping))
        .route("/api/devices", get(get_devices))
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
