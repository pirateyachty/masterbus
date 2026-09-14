use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use masterbus_tools::mapping::Mapping;

struct AppState {
    mapping_path: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mapping_path = std::env::var_os("MAPPING")
        .map(PathBuf::from)
        .map_or_else(
            || masterbus::FileConfig::load_or_create().map(|c| c.mapping_path()),
            Ok,
        )?;

    println!("mapping: {}", mapping_path.display());

    let state = Arc::new(AppState { mapping_path });

    let app = Router::new()
        .route("/", get(root))
        .route("/api/mapping", get(get_mapping))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3008").await?;

    println!("masterbus-web listening on http://0.0.0.0:3008");

    axum::serve(listener, app).await?;

    Ok(())
}

async fn root() -> &'static str {
    "MasterBus Web"
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