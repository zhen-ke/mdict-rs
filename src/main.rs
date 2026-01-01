use crate::config::{MDX_FILES, static_path};
use crate::handlers::{
    handle_lucky, handle_query, handle_resource, handle_suggest, handle_trace,
    handle_dict_list, handle_dict_style, handle_dict_script
};
use crate::indexing::indexing;

use axum::{
    Router,
    routing::{get, post},
};
use std::error::Error;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod handlers;
mod indexing;
mod lucky;
mod mdict;
mod query;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // 初始化日志系统
    tracing_subscriber::registry()
        .with(EnvFilter::new("info"))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 解析mdx到sqlite数据库
    indexing(&MDX_FILES, false).expect("indexing failed");

    // 静态文件服务
    let static_dir = static_path()?;
    info!("Serving static files from: {:?}", static_dir);

    let app = Router::new()
        .route("/query", post(handle_query))
        .route("/suggest", get(handle_suggest))
        .route("/lucky", get(handle_lucky))
        .route("/trace", get(handle_trace))
        // Dictionary config API
        .route("/api/dicts", get(handle_dict_list))
        .route("/api/dict/style", get(handle_dict_style))
        .route("/api/dict/script", get(handle_dict_script))
        // 词典资源处理 (音频、图片等)
        .route("/{*path}", get(handle_resource))
        // 静态文件服务 (index.html, css, js)
        .fallback_service(ServeDir::new(&static_dir))
        .layer(TraceLayer::new_for_http());

    let port = 8181;
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8181").await.unwrap();

    info!("app serve on http://localhost:{}", port);

    axum::serve(listener, app).await?;

    Ok(())
}
