use crate::app_state::{AppState, DictCatalog};
use crate::config::{DictConfig, get_dict_dir, scan_dict_files, static_path};
use crate::handlers::{
    handle_dict_audio, handle_dict_entry, handle_dict_list, handle_dict_res, handle_dict_resource,
    handle_dict_script, handle_dict_style, handle_favorites_add, handle_favorites_clear,
    handle_favorites_list, handle_favorites_remove, handle_index_status, handle_lucky,
    handle_query, handle_resource, handle_suggest, handle_suggest_fuzzy, handle_trace,
};
use mdict_core::indexing::{IndexJob, db_path, index_up_to_date, indexing};

use axum::{
    Router,
    routing::{get, post},
};
use std::collections::HashSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

mod app_state;
mod config;
mod favorites;
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
        build_index_jobs(&dict_files)
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
        .route("/api/favorites", get(handle_favorites_list))
        .route("/api/favorites", post(handle_favorites_add))
        .route("/api/favorites", axum::routing::delete(handle_favorites_clear))
        .route(
            "/api/favorites/{word}",
            axum::routing::delete(handle_favorites_remove),
        )
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

/// 为词典文件列表构建索引任务：FTS 开关与分词器取自同名 .toml 配置。
/// 启动全量索引与热更新增量索引共用，保证两处判定一致。
fn build_index_jobs(dict_files: &[PathBuf]) -> Vec<IndexJob> {
    dict_files
        .iter()
        .map(|file| {
            let is_text_dict = !file
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("mdd"));
            let cfg = DictConfig::load(file);
            let fts_enabled =
                is_text_dict && cfg.as_ref().map(|c| c.is_fts_enabled()).unwrap_or(true);
            let fts_tokenizer = cfg.as_ref().map(|c| c.fts_tokenizer()).unwrap_or_default();
            IndexJob::new(file.clone(), fts_enabled, fts_tokenizer)
        })
        .collect()
}

/// 后台索引重试循环。初始一次性尝试，失败则按 5s / 20s / 60s 退避重试两轮，
/// 依然失败则把最后一次错误写入 [`AppState::record_index_failure`]，供
/// `/api/index/status` 暴露。退避用 `tokio::time::sleep` 而非阻塞 `thread::sleep`，
/// 不占用 blocking 线程。
///
/// 重试采用「按文件重试」策略：每一轮只重试上一轮仍失败的子集，避免对已
/// 成功的词典重复建索引。`indexing()` 本身用 rayon 并行按文件调度。
/// 每轮尝试持有 `reload_lock`，与热更新 watcher 互斥，避免同一词典被
/// 并发重建索引。
async fn indexing_with_retry(
    jobs: Vec<IndexJob>,
    state: AppState,
    index_pool: std::sync::Arc<rayon::ThreadPool>,
) {
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
        let result = {
            // 与热更新 watcher 串行化：索引文件期间不许并发重载同一词典。
            let _reload_guard = state.reload_lock().lock().await;
            let pool_ref = index_pool.clone();
            tokio::task::spawn_blocking(move || indexing(&snapshot, false, Some(&pool_ref)))
                .await
                .map_err(|join| {
                    vec![(
                        PathBuf::new(),
                        anyhow::anyhow!("indexing task join error: {join}"),
                    )]
                })
        };

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

// =========================== 词典目录热更新 ===========================

/// 是否启用词典目录热更新。默认启用；设 `MDICT_HOT_RELOAD=0` 可关闭（例如在
/// 卷声不兼容或审调试时）。
fn needs_dict_watcher(dict_dir: &Path) -> bool {
    if !dict_dir.is_dir() {
        return false;
    }
    std::env::var("MDICT_HOT_RELOAD")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .map(|v| v != 0)
        .unwrap_or(true)
}

/// 是否关心该扩展名的变更事件。只放行词典源文件与配置文件；
/// 索引产物 `.db` 必须过滤，否则「重载 → 建索引 → .db 事件 → 重载」
/// 会形成死循环。
fn is_relevant_extension(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        let e = e.to_string_lossy().to_ascii_lowercase();
        e == "mdx" || e == "mdd" || e == "toml"
    })
}

/// notify 事件是否值得触发一次重载。
fn watcher_event_relevant(event: &notify::Event) -> bool {
    event.paths.iter().any(|p| is_relevant_extension(p))
}

/// 事件驱动 + 周期兜底扫描的词典目录热更新循环。
///
/// - notify 递归监听，相关事件（.mdx/.mdd/.toml）去抖 `RELOAD_DEBOUNCE`
///   后执行一次全量重载（见 [`reload_dicts`]）。
/// - 每 `RELOAD_FULL_SCAN_INTERVAL` 周期全量重载兜底：覆盖 notify 漏事件
///   （inotify 上限、网络卷等）与启动前的存量变化；无变化时重载是纯
///   no-op（`index_up_to_date` 短路，不重建索引）。
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(800);
const RELOAD_FULL_SCAN_INTERVAL: Duration = Duration::from_secs(60);

async fn dict_watcher(
    dict_dir: PathBuf,
    state: AppState,
    index_pool: std::sync::Arc<rayon::ThreadPool>,
) {
    use notify::{RecursiveMode, Watcher};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();
    let mut watcher = match notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    }) {
        Ok(w) => Some(w),
        Err(e) => {
            error!("failed to start dict file watcher, falling back to periodic scan only: {e}");
            None
        }
    };
    if let Some(w) = watcher.as_mut() {
        match w.watch(&dict_dir, RecursiveMode::Recursive) {
            Ok(()) => info!("dict hot-reload watcher started on {:?}", dict_dir),
            Err(e) => {
                // watch 失败时 watcher 对象本身不注册任何监听，无害；周期
                // 扫描兜底仍生效。
                error!(
                    "failed to watch dict dir {:?}, falling back to periodic scan only: {e}",
                    dict_dir
                );
            }
        }
    }

    let mut last_full_scan = Instant::now();
    loop {
        // 周期兜底扫描到期 → 直接全量重载（无变化时快速 no-op）。
        if last_full_scan.elapsed() >= RELOAD_FULL_SCAN_INTERVAL {
            reload_dicts(&state, &index_pool, &dict_dir, &HashSet::new()).await;
            last_full_scan = Instant::now();
            continue;
        }

        // 等待下一个相关事件（或周期到期返回重扫）。
        let until_full = RELOAD_FULL_SCAN_INTERVAL.saturating_sub(last_full_scan.elapsed());
        let event = match tokio::time::timeout(until_full, rx.recv()).await {
            Ok(Some(ev)) => ev,
            _ => continue,
        };
        if !watcher_event_relevant(&event) {
            continue;
        }

        // 去抖：吞掉随后 DEBOUNCE 窗口内的相关事件，避免编辑器写文件
        // （多次 Modify + Create）触发连环重载；窗口内路径并入 affected。
        let mut affected: HashSet<PathBuf> = event
            .paths
            .iter()
            .filter(|p| is_relevant_extension(p))
            .cloned()
            .collect();
        loop {
            match tokio::time::timeout(RELOAD_DEBOUNCE, rx.recv()).await {
                Ok(Some(ev)) if watcher_event_relevant(&ev) => {
                    for p in ev.paths {
                        if is_relevant_extension(&p) {
                            affected.insert(p);
                        }
                    }
                }
                _ => break,
            }
        }
        info!(
            "dict dir changed ({} relevant path(s)), reloading",
            affected.len()
        );
        reload_dicts(&state, &index_pool, &dict_dir, &affected).await;
        last_full_scan = Instant::now();
    }
}

/// 执行一次全量重载（热更新核心，见 [`dict_watcher`]）：
///
/// 1. 重扫词典目录并构建新 catalog（含 .toml 配置与 CSS/JS 内容）。
/// 2. 与旧 catalog 差集 + `index_up_to_date` 判定变更集（新增/删除/源文件变化）。
/// 3. 先轮换变更/删除词典的运行时资源（连接池、mmap reader）再重建索引：
///    旧池在 Windows 上会占住 .db 文件导致删不掉，且旧 mmap 映射的是旧
///    内容 inode，重建后必须让新请求开新 reader。
/// 4. 在专用低优索引池上增量重建变更词典的索引；失败记入 `/api/index/status`
///    （下一轮周期扫描会按 mtime 差异自动重试）。
/// 5. 清理被删除词典的残留 .db。
/// 6. 失效受影响词典的缓存（entry/resource/negative 三层；聚合条目全清）。
/// 7. 原子换新 catalog（arc-swap），此后新请求看到新词典表。
///
/// 全程持有 `AppState::reload_lock`，与启动重试循环互斥。
/// `affected_events` 为去抖窗口内的 notify 事件路径（含 .toml）：仅配置变更
/// 时源文件未动、无需重建索引，但聚合 HTML 里嵌了 CSS/JS，必须失效缓存。
async fn reload_dicts(
    state: &AppState,
    index_pool: &std::sync::Arc<rayon::ThreadPool>,
    dict_dir: &Path,
    affected_events: &HashSet<PathBuf>,
) {
    let _reload_guard = state.reload_lock().lock().await;

    let new_files = scan_dict_files(dict_dir);
    let new_catalog = DictCatalog::from_dict_files(&new_files, dict_dir);

    let old_files: HashSet<PathBuf> = state.all_dict_files().into_iter().collect();
    let new_set: HashSet<PathBuf> = new_files.iter().cloned().collect();

    // 变更集：新增 / 删除 / 索引已过期（源文件 mtime 或 size 变化）。
    let mut changed: HashSet<PathBuf> = HashSet::new();
    let mut removed: HashSet<PathBuf> = HashSet::new();
    for f in old_files.iter().chain(new_files.iter()) {
        if !new_set.contains(f) {
            removed.insert(f.clone());
        } else if !old_files.contains(f) || !index_up_to_date(f, &db_path(f)).unwrap_or(false) {
            changed.insert(f.clone());
        }
    }

    let mut rotated: HashSet<PathBuf> = changed.clone();
    rotated.extend(removed.iter().cloned());

    // 先轮换运行时资源（在途查询持有的 Arc 克隆不受影响），再重建索引。
    state.drop_runtime_for(&rotated);

    // 增量重建变更词典的索引（仅变更子集；新增词典同样在此首建）。
    if !changed.is_empty() {
        let changed_files: Vec<PathBuf> = changed.iter().cloned().collect();
        let jobs = build_index_jobs(&changed_files);
        let pool_ref = index_pool.clone();
        let jobs_for_task = jobs.clone();
        let result =
            tokio::task::spawn_blocking(move || indexing(&jobs_for_task, false, Some(&pool_ref)))
                .await
                .map_err(|join| {
                    vec![(
                        PathBuf::new(),
                        anyhow::anyhow!("indexing task join error: {join}"),
                    )]
                });
        match result {
            Ok(Ok(())) => {
                for job in &jobs {
                    state.clear_index_failure(&job.path);
                }
            }
            Ok(Err(failures)) | Err(failures) => {
                for (path, err) in &failures {
                    if path.as_os_str().is_empty() {
                        error!("hot-reload indexing join error: {}", err);
                        continue;
                    }
                    warn!("hot-reload indexing failed for {:?}: {}", path, err);
                    state.record_index_failure(path.clone(), err.to_string(), 1);
                }
            }
        }
    }

    // 清理被删除词典的残留索引。
    for f in &removed {
        state.clear_index_failure(f);
        let db = db_path(f);
        if db.exists() {
            info!("removing stale index for removed dict {:?}", db);
            if let Err(e) = std::fs::remove_file(&db) {
                warn!("failed to remove stale index {:?}: {}", db, e);
            }
        }
    }

    // 缓存失效：变更/删除词典的 id（旧 catalog）+ 新增词典的 id（新 catalog）
    // + notify 事件中 .toml 对应的词典（仅配置变更，源文件未动）。
    let mut affected_ids: HashSet<String> = HashSet::new();
    for f in &rotated {
        if let Some(id) = state.get_dict_id(f) {
            affected_ids.insert(id);
        }
    }
    for f in &changed {
        if let Some(id) = new_catalog.id_of_path(f) {
            affected_ids.insert(id.to_string());
        }
    }
    for p in affected_events {
        if !is_relevant_extension(p) {
            continue;
        }
        let Some(stem) = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else {
            continue;
        };
        for f in &new_files {
            if f.file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .is_some_and(|s| s == stem)
            {
                if let Some(id) = new_catalog.id_of_path(f) {
                    affected_ids.insert(id.to_string());
                }
                break;
            }
        }
    }
    state.invalidate_caches_for_dicts(&affected_ids);

    // 原子换新编目：此后新请求看到新词典表。
    state.reload_catalog(new_catalog);
    info!(
        "dict catalog reloaded: {} text dict(s), {} changed, {} removed",
        state.dict_text_files().len(),
        changed.len(),
        removed.len()
    );
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
