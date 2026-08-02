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
use std::path::PathBuf;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
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

    info!("Using dict dir: {:?}", dict_dir);
    info!("Serving static files from: {:?}", static_dir);

    // 构建索引任务表：需在 `AppState::new` 消费 `dict_files` 之前完成。
    // FTS 仅对文本词典（.mdx）有意义；资源词典（.mdd）强制关闭。
    // 单词典可通过同名 .toml 配置关闭 FTS。
    let jobs: Vec<IndexJob> = if dict_files.is_empty() {
        Vec::new()
    } else {
        dict_files
            .iter()
            .map(|file| {
                let is_text_dict = !file
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("mdd"));
                let fts_enabled = is_text_dict
                    && DictConfig::load(file)
                        .map(|cfg| cfg.is_fts_enabled())
                        .unwrap_or(true);
                let fts_tokenizer = DictConfig::load(file)
                    .map(|cfg| cfg.fts_tokenizer())
                    .unwrap_or(mdict_core::indexing::FtsTokenizer::Unicode61);
                IndexJob::new(file.clone(), fts_enabled, fts_tokenizer)
            })
            .collect()
    };
    if !jobs.is_empty() {
        info!(
            "Scheduling background index ensure for {} dictionary files",
            jobs.len()
        );
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

    let state = AppState::new(dict_dir.clone(), static_dir.clone(), dict_files);

    // 后台索引固定交给独立 tokio task：采用异步重试循环（5s / 20s / 60s），
    // 成功的词典从失败 map 清除；重试仍失败时把原因写入 AppState 供
    // `/api/index/status` 暴露，避免此前 fire-and-forget 静默 503。
    // 构建后台建索引专用的 rayon 线程池：线程数可经 `MDICT_INDEX_THREADS`
    // 限制，每线程启动时降到低优先级，避免在弱设备（树莓派等）上建索引睡满核、
    // 持续抢占 Web 请求资源。也在热更新路径复用（见 `dict_watcher` 的单文件 ensure_index
    // 直接跑在该池上而非全局池）。
    let index_pool = build_index_thread_pool();

    if !jobs.is_empty() {
        tokio::spawn(indexing_with_retry(
            jobs,
            state.clone(),
            index_pool.clone(),
        ));
    }
    // 监听词典目录热更新（新增/变更 .mdx/.mdd/.toml → 增量建索引并热替换 catalog）。
    if needs_dict_watcher(&dict_dir) {
        tokio::spawn(dict_watcher(dict_dir, state.clone(), index_pool.clone()));
    }

    let app = Router::new()
        // 轻量存活探针：容器编排 / NAS Health Check 使用，不走 AppState、不占 Semaphore。响应体极小。
        .route("/health", get(|| async { "ok\n" }))
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
        // 层顺序：请求 Compression -> Trace -> handler，响应 handler -> Trace -> Compression。
        // Compression 作为最外层，对全部响应（含错误响应）走 Accept-Encoding 协商压缩；
        // TraceLayer 在内层，其 span 包住 handler 时间（不抖 Compression 开销）。
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .with_state(state);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8181u16);
    let host = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    let display_host = if host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        &host
    };
    info!("app serve on http://{}:{}", display_host, port);

    // 优雅停机：收到 Ctrl-C / SIGTERM 后，拒绝新连接、等待在途查询完成再退出。
    // axum::serve 默认 30s 超时。避免 docker stop 的内核 SIGKILL 硬丝在途请求。
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// 后台索引重试循环。初始一次性尝试，失败则按 5s / 20s / 60s 退避重试两轮，
/// 依然失败则把最后一次错误写入 [`AppState::record_index_failure`]，供
/// `/api/index/status` 暴露。退避用 `tokio::time::sleep` 而非阻塞 `thread::sleep`，
/// 不占用 blocking 线程。
///
/// 重试采用「按文件重试」策略：每一轮只重试上一轮仍失败的子集，避免对已
/// 成功的词典重复建索引。`indexing()` 本身用 rayon 并行按文件调度。
async fn indexing_with_retry(
    jobs: Vec<IndexJob>,
    state: AppState,
    index_pool: std::sync::Arc<rayon::ThreadPool>,
) {
    use std::time::Duration;

    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFFS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(20)];

    let mut pending: Vec<IndexJob> = jobs;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let snapshot = pending.clone();
        tracing::info!(
            "Background indexing attempt {} of {} ({} files)",
            attempts,
            MAX_ATTEMPTS,
            snapshot.len()
        );
        // 复用 indexing 的 rayon 并行按件调度；join panic 也化为同一条 ETL。
        // 走专用 `index_pool`（低优先级、线程数受限），而非 rayon 全局池。
        let pool_ref = index_pool.clone();
        let result = tokio::task::spawn_blocking(move || {
            indexing(&snapshot, false, Some(&pool_ref))
        })
        .await
        .map_err(|join| {
            vec![(
                PathBuf::new(),
                anyhow::anyhow!("indexing task join error: {join}"),
            )]
        });

        match result {
            Ok(Ok(())) => {
                // 本轮全部文件成功：把它们从失败 map 中清除，供 /api/index/status 显示。
                for job in &pending {
                    state.clear_index_failure(&job.path);
                }
                info!("Background indexing completed successfully");
                return;
            }
            Ok(Err(failures)) | Err(failures) => {
                // join panic 路径会产生一条 path=="" 的合成记录，其它是真实文件路径。
                for (path, err) in &failures {
                    if path.as_os_str().is_empty() {
                        error!("Background indexing join error: {}", err);
                        continue;
                    }
                    // 中间轮也写入，前端可在重试期间看到当前原因+尝试次数。
                    warn!(
                        "Background indexing failed for {:?} on attempt {}: {}",
                        path, attempts, err
                    );
                    state.record_index_failure(path.clone(), err.to_string(), attempts);
                }
                if attempts >= MAX_ATTEMPTS {
                    for (path, err) in &failures {
                        if path.as_os_str().is_empty() {
                            continue;
                        }
                        error!(
                            "Background indexing gave up on {:?} after {} attempts: {}",
                            path, attempts, err
                        );
                    }
                    return;
                }
                let backoff = BACKOFFS[(attempts - 1) as usize];
                warn!(
                    "Background indexing still failing for {} dict(s); retrying in {:?}",
                    failures.len(),
                    backoff
                );
                // 只重试仍失败的真实词典子集
                let failed_paths: std::collections::HashSet<PathBuf> = failures
                    .iter()
                    .map(|(p, _)| p.clone())
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect();
                pending.retain(|j| failed_paths.contains(&j.path));
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

// =========================== 后台建索引线程池 ===========================

/// 后台建索引的线程数。优先 `MDICT_INDEX_THREADS` 环境变量；非正或未设 → `None`，
/// 表示「不限制」交由 rayon 按可用核自适配。
fn index_threads_config() -> Option<usize> {
    std::env::var("MDICT_INDEX_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
}

/// 尝试把当前线程优先级降到最低。best-effort：无权泶（非 root 降低 nice 通常
/// 可，提升不可）会安静报错并上车。跨平台为 Linux/macOS/Windows 的低优先映射。
fn try_lower_thread_priority() {
    use thread_priority::{ThreadPriority, set_current_thread_priority};
    if let Err(e) = set_current_thread_priority(ThreadPriority::Min) {
        tracing::debug!("could not lower indexing thread priority: {e}");
    }
}

/// 构建后台建索引专用的 rayon 线程池：线程数可配（`MDICT_INDEX_THREADS`），
/// 每个线程启动时降到低优先级，避免在树莓派等弱设备上建索引吃满核抢占
/// Web 查询线程。返回 `Arc<ThreadPool>` 以便后续热更新 watcher 复用同一池。
fn build_index_thread_pool() -> std::sync::Arc<rayon::ThreadPool> {
    let mut builder = rayon::ThreadPoolBuilder::new().thread_name(|i| format!("mdict-index-{i}"));
    if let Some(n) = index_threads_config() {
        builder = builder.num_threads(n);
        info!("index pool limited to {n} threads (MDICT_INDEX_THREADS)");
    } else {
        info!("index pool: no thread limit (MDICT_INDEX_THREADS unset); rayon self-adapts");
    }
    // 每线程启动时降优先级。闭包跨线程被 clone，但仅读周边不可变环境。
    builder = builder.start_handler(move |_| try_lower_thread_priority());

    match builder.build() {
        Ok(pool) => std::sync::Arc::new(pool),
        Err(e) => {
            // 进手册不会发生（threads 配置合法），但作为竟竟卫仍给出明确错误。
            error!("failed to build dedicated index pool, falling back to global rayon pool: {e}");
            // 退回：一个以身作则的“什么都不限制”池仍是独立于全局右侧的 right 语义。
            // 按 rayon：build 失败律不可复用为全局，以不变价仍 build 一默认池。
            std::sync::Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .thread_name(|i| format!("mdict-index-{i}"))
                    .build()
                    .expect("rayon default pool build must not fail"),
            )
        }
    }
}

// =========================== 词典目录热更新（stub，后续填充） ===========================

/// 是否启用词典目录热更新。默认启用；设 `MDICT_HOT_RELOAD=0` 可关闭（例如在
/// 卷声不兼容或审调试时）。
fn needs_dict_watcher(dict_dir: &std::path::Path) -> bool {
    if !dict_dir.is_dir() {
        return false;
    }
    std::env::var("MDICT_HOT_RELOAD")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .map(|v| v != 0)
        .unwrap_or(true)
}

/// 监听词典目录新增/变更的 .mdx/.mdd/.toml，增量建索引并热替换 catalog。
///
/// 完整实现见后续应用（需要 AppState::reload_catalog、notify 事件循环、
/// arc-swap catalog）。此骨架仅保持任务存活、按周期重扫以保证构建绿。
#[allow(unused_variables)]
async fn dict_watcher(
    dict_dir: std::path::PathBuf,
    state: AppState,
    index_pool: std::sync::Arc<rayon::ThreadPool>,
) {
    use std::time::Duration;
    tracing::info!("dict hot-reload watcher started on {:?}", dict_dir);
    loop {
        // HACK: stub 暂以惰性轮询 + 长间隔代替 notify（实现真实监听见后续检查）。
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// 监听 Ctrl-C 与（Unix）SIGTERM，任一到达即触发优雅停机。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received Ctrl-C, initiating graceful shutdown"),
        _ = terminate => info!("Received SIGTERM, initiating graceful shutdown"),
    }
}
