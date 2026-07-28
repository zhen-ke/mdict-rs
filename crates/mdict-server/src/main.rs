use crate::app_state::AppState;
use crate::config::{DictConfig, get_dict_dir, scan_dict_files, static_path};
use crate::handlers::{
    handle_dict_audio, handle_dict_entry, handle_dict_list, handle_dict_res, handle_dict_resource,
    handle_dict_script, handle_dict_style, handle_index_status, handle_lucky, handle_query,
    handle_resource, handle_suggest, handle_suggest_fuzzy, handle_trace,
};
use mdict_core::indexing::{IndexJob, db_path, indexing};

use axum::{
    Router,
    routing::{get, post},
};
use std::error::Error;
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

mod app_state;
mod config;
mod handlers;
mod lucky;
mod query;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // 初始化日志系统（支持 RUST_LOG 环境变量覆盖，默认 info）
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 词典目录与静态资源目录
    let dict_dir = get_dict_dir();
    let static_dir = static_path()?;

    // 扫描词典文件
    let dict_files = scan_dict_files(&dict_dir);

    // 确保索引已生成（首次启动会花时间；后续有 .db 则很快）
    if !dict_files.is_empty() {
        // FTS 仅对文本词典（.mdx）有意义；资源词典（.mdd）强制关闭。
        // 单词典可通过同名 .toml 配置关闭 FTS。
        let jobs: Vec<IndexJob> = dict_files
            .iter()
            .map(|file| {
                let is_text_dict = !file
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("mdd"));
                let fts_enabled = is_text_dict
                    && DictConfig::load(file)
                        .map(|cfg| cfg.is_fts_enabled())
                        .unwrap_or(true);
                IndexJob::new(file.clone(), fts_enabled)
            })
            .collect();
        info!(
            "Scheduling background index ensure for {} dictionary files",
            jobs.len()
        );
        tokio::task::spawn_blocking(move || match indexing(&jobs, false) {
            Ok(()) => info!("Background indexing completed"),
            Err(e) => error!("Background indexing finished with errors: {}", e),
        });

        let missing: usize = dict_files
            .iter()
            .filter(|file| !db_path(file).exists())
            .count();
        if missing > 0 {
            info!(
                "{} dictionary indexes are pending and will be available after background indexing",
                missing
            );
        }
    }

    info!("Using dict dir: {:?}", dict_dir);
    info!("Serving static files from: {:?}", static_dir);

    let state = AppState::new(dict_dir, static_dir.clone(), dict_files);

    let app = Router::new()
        .route("/query", post(handle_query))
        .route("/suggest", get(handle_suggest))
        .route("/suggest/fuzzy", get(handle_suggest_fuzzy))
        .route("/lucky", get(handle_lucky))
        .route("/trace", get(handle_trace))
        // Dictionary config API
        .route("/api/dicts", get(handle_dict_list))
        .route("/api/index/status", get(handle_index_status))
        .route("/api/dict/style", get(handle_dict_style))
        .route("/api/dict/script", get(handle_dict_script))
        .route("/dict/{id}/entry/{*word}", get(handle_dict_entry))
        .route("/dict/{id}/res/{*path}", get(handle_dict_res))
        .route("/dict/{id}/audio/{*path}", get(handle_dict_audio))
        // Legacy route compatibility
        .route("/resource/{id}/{*path}", get(handle_dict_resource))
        // 静态文件 + 词典资源处理 (音频、图片等)
        .fallback(handle_resource)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8181u16);
    let host = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!("app serve on http://{}:{}", host, port);

    axum::serve(listener, app).await?;

    Ok(())
}
