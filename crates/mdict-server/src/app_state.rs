use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::body::Bytes;
use moka::sync::Cache;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use ripemd::{Digest, Ripemd160};
use rusqlite::OpenFlags;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::{DictConfig, DictInfo};
use mdict_core::mdict::reader::{MdxReader, per_reader_cache_budget};

/// Entry cache 的字节预算上限。moka 的 `max_capacity` 配合 `weigher` 后语义是
/// “所有缓存项的 weight 之和 ≤ max_capacity”，故这里把 `data.len()` 作为 weight，
/// 整个 entry cache 的内存峰值被钉死在这个量级，与单个聚合页大小无关。
const ENTRY_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// Resource cache 的字节预算上限（音频/图片/css…），按字节封顶。
const RESOURCE_CACHE_BYTES: u64 = 128 * 1024 * 1024;
/// 条目式兜底容量（moka 同时受 `max_capacity` 与 `weigher` 双重约束，取最小）。
const RESOURCE_CACHE_MAX_ENTRIES: u64 = 4096;
const NEGATIVE_CACHE_SIZE: u64 = 1024;
/// 阻塞查询并发的硬上界（也作为 `available_parallelism` 取不到值时的兑回路值）。
///
/// 实际默认值在运行时自适应为 `min(64, max(8, n_cpus))`——见
/// [`runtime_max_concurrent_blocking_queries`]，使「请求并发与单请求 rayon 并行」
/// 矩阵更均衡，避免超额并发掘起过多的 blocking 线程轮转开销。仍可被环境变量
/// `MDICT_MAX_CONCURRENT_BLOCKING_QUERIES` 显式覆盖。
const DEFAULT_MAX_CONCURRENT_BLOCKING_QUERIES: usize = 64;

/// 运行时计算的自适配并发上限：`min(64, max(8, n_cpus))`。
fn runtime_max_concurrent_blocking_queries() -> usize {
    let n = std::thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(DEFAULT_MAX_CONCURRENT_BLOCKING_QUERIES);
    n.clamp(8, DEFAULT_MAX_CONCURRENT_BLOCKING_QUERIES)
}
const ENTRY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const RESOURCE_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(20);

#[derive(Clone)]
struct CachedPayload {
    data: Bytes,
    /// `Arc<str>` 让缓存命中路径（`get_*_cached` 返回给 handler）只做一次
    /// arc 引用计数加，而不是 `String` 的堆分配+拷贝。content_type 通常是
    /// `"text/html"`/`"image/png"` 这类短串，跨条目大量共享，arc clone 廉价。
    content_type: Arc<str>,
}

struct DictCatalog {
    dict_text_files: Vec<PathBuf>,
    dict_resource_files: Vec<PathBuf>,
    dict_configs_by_path: HashMap<PathBuf, DictConfig>,
    dict_configs_by_id: HashMap<String, DictConfig>,
    id_to_primary_text: HashMap<String, PathBuf>,
    dict_id_map: HashMap<String, Vec<PathBuf>>,
    path_to_id: HashMap<PathBuf, String>,
}

impl DictCatalog {
    fn from_dict_files(dict_files: &[PathBuf]) -> Self {
        let dict_text_files: Vec<PathBuf> = dict_files
            .iter()
            .filter(|p| AppState::is_mdx_file(p))
            .cloned()
            .collect();

        let mut mdd = Vec::new();
        let mut mdx = Vec::new();
        for file in dict_files {
            if AppState::is_mdd_file(file) {
                mdd.push(file.clone());
            } else {
                mdx.push(file.clone());
            }
        }
        mdd.extend(mdx);
        let dict_resource_files = mdd;

        let mut dict_configs_by_path = HashMap::new();
        for file in &dict_text_files {
            if let Some(cfg) = DictConfig::load(file) {
                dict_configs_by_path.insert(file.clone(), cfg);
            }
        }

        let mut dict_id_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut path_to_id = HashMap::new();
        let mut id_to_logical = HashMap::new();

        for file in dict_files {
            let logical_key = AppState::logical_dict_key(file);
            let id = AppState::allocate_dict_id(&logical_key, &mut id_to_logical);
            dict_id_map
                .entry(id.clone())
                .or_default()
                .push(file.clone());
            path_to_id.insert(file.clone(), id);
        }

        let mut id_to_primary_text = HashMap::new();
        let mut dict_configs_by_id = HashMap::new();
        for (id, files) in &dict_id_map {
            let mut text_files: Vec<PathBuf> = files
                .iter()
                .filter(|f| AppState::is_mdx_file(f))
                .cloned()
                .collect();
            text_files.sort();

            if let Some(primary_text) = text_files.into_iter().next() {
                id_to_primary_text.insert(id.clone(), primary_text.clone());
                if let Some(cfg) = dict_configs_by_path.get(&primary_text) {
                    dict_configs_by_id.insert(id.clone(), cfg.clone());
                }
            }
        }

        Self {
            dict_text_files,
            dict_resource_files,
            dict_configs_by_path,
            dict_configs_by_id,
            id_to_primary_text,
            dict_id_map,
            path_to_id,
        }
    }
}

struct RuntimeState {
    db_pools: RwLock<HashMap<PathBuf, Pool<SqliteConnectionManager>>>,
    mdx_readers: RwLock<HashMap<PathBuf, Arc<MdxReader>>>,
    blocking_query_slots: Arc<Semaphore>,
    /// Lock-free concurrent LRU cache for entry lookups (moka).
    entry_cache: Cache<String, CachedPayload>,
    /// Lock-free concurrent LRU cache for resource lookups (moka).
    resource_cache: Cache<String, CachedPayload>,
    /// Lock-free concurrent LRU cache for negative (miss) entries (moka).
    negative_cache: Cache<String, ()>,
}

impl RuntimeState {
    fn new(max_concurrent_blocking_queries: usize) -> Self {
        Self {
            db_pools: RwLock::new(HashMap::new()),
            mdx_readers: RwLock::new(HashMap::new()),
            blocking_query_slots: Arc::new(Semaphore::new(max_concurrent_blocking_queries)),
            entry_cache: Cache::builder()
                // 按字节封顶：每项的 weight = data.len()，几条巨型聚合页也不会
                // 让 entry cache 漂出 ENTRY_CACHE_BYTES。
                .max_capacity(ENTRY_CACHE_BYTES)
                .weigher(|_k, v: &CachedPayload| u32::try_from(v.data.len()).unwrap_or(u32::MAX))
                .time_to_live(ENTRY_CACHE_TTL)
                .build(),
            resource_cache: Cache::builder()
                .max_capacity(RESOURCE_CACHE_BYTES)
                // 字节预算是真正的约束；同时再设一个条目数上限，防止海量极小
                // 资源（icon/小字体）把条数推到无界（moka 取两约束的较小值）。
                .weigher(|_k, v: &CachedPayload| u32::try_from(v.data.len()).unwrap_or(u32::MAX))
                .time_to_live(RESOURCE_CACHE_TTL)
                .max_capacity(RESOURCE_CACHE_MAX_ENTRIES)
                .build(),
            negative_cache: Cache::builder()
                .max_capacity(NEGATIVE_CACHE_SIZE)
                .time_to_live(NEGATIVE_CACHE_TTL)
                .build(),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    dict_dir: Arc<PathBuf>,
    static_dir: Arc<PathBuf>,
    catalog: Arc<DictCatalog>,
    runtime: Arc<RuntimeState>,
}

impl AppState {
    pub fn new(dict_dir: PathBuf, static_dir: PathBuf, dict_files: Vec<PathBuf>) -> Self {
        // 显式环境变量优先；未设时走运行时自适配（≈ CPU 并行度，clamped 8..64）。
        let max_concurrent_blocking_queries =
            std::env::var("MDICT_MAX_CONCURRENT_BLOCKING_QUERIES")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or_else(runtime_max_concurrent_blocking_queries);

        let catalog = DictCatalog::from_dict_files(&dict_files);
        let runtime = RuntimeState::new(max_concurrent_blocking_queries);

        Self {
            dict_dir: Arc::new(dict_dir),
            static_dir: Arc::new(static_dir),
            catalog: Arc::new(catalog),
            runtime: Arc::new(runtime),
        }
    }

    pub fn dict_dir(&self) -> &Path {
        &self.dict_dir
    }

    pub fn static_dir(&self) -> &Path {
        &self.static_dir
    }

    pub fn dict_text_files(&self) -> &[PathBuf] {
        &self.catalog.dict_text_files
    }

    pub fn dict_resource_files(&self) -> &[PathBuf] {
        &self.catalog.dict_resource_files
    }

    pub fn get_dict_id(&self, path: &Path) -> Option<String> {
        self.catalog.path_to_id.get(path).cloned()
    }

    pub fn get_dict_files(&self, id: &str) -> Option<&Vec<PathBuf>> {
        self.catalog.dict_id_map.get(id)
    }

    pub fn get_dict_text_files_by_id(&self, id: &str) -> Vec<PathBuf> {
        self.get_dict_files(id)
            .map(|files| {
                files
                    .iter()
                    .filter(|f| Self::is_mdx_file(f))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_dict_resource_files_by_id(&self, id: &str) -> Vec<PathBuf> {
        let Some(files) = self.get_dict_files(id) else {
            return Vec::new();
        };

        let mut seen = HashSet::new();
        let mut mdd = Vec::new();
        let mut mdx = Vec::new();

        for file in files {
            if !seen.insert(file.clone()) {
                continue;
            }
            if Self::is_mdd_file(file) {
                mdd.push(file.clone());
            } else if Self::is_mdx_file(file) {
                mdx.push(file.clone());
            }
        }

        mdd.extend(mdx);
        mdd
    }

    pub fn get_dict_config(&self, dict_id: &str) -> Option<DictConfig> {
        if let Some(cfg) = self.catalog.dict_configs_by_id.get(dict_id) {
            return Some(cfg.clone());
        }

        let path = PathBuf::from(dict_id);
        self.catalog.dict_configs_by_path.get(&path).cloned()
    }

    pub fn get_all_dict_info(&self) -> Vec<DictInfo> {
        let mut ids: Vec<&String> = self.catalog.id_to_primary_text.keys().collect();
        ids.sort();

        ids.into_iter()
            .filter_map(|id| {
                let file = self.catalog.id_to_primary_text.get(id)?;
                let default_config = DictConfig::default();
                let cfg = self
                    .catalog
                    .dict_configs_by_path
                    .get(file)
                    .unwrap_or(&default_config);
                Some(DictInfo {
                    id: id.clone(),
                    name: cfg.get_display_name(file),
                    description: cfg.description.clone(),
                    container_class: cfg.container_class.clone(),
                    has_css: cfg.css.is_some(),
                    has_js: cfg.js.is_some(),
                    fts_enabled: cfg.is_fts_enabled(),
                })
            })
            .collect()
    }

    pub fn get_dict_display_name(&self, dict_path: &Path) -> String {
        let default_config = DictConfig::default();
        self.catalog
            .dict_configs_by_path
            .get(dict_path)
            .unwrap_or(&default_config)
            .get_display_name(dict_path)
    }

    pub fn get_dict_container_class(&self, dict_path: &Path) -> Option<String> {
        self.catalog
            .dict_configs_by_path
            .get(dict_path)
            .and_then(|cfg| cfg.container_class.clone())
    }

    fn db_path(dict_file: &Path) -> PathBuf {
        PathBuf::from(format!("{}.db", dict_file.to_string_lossy()))
    }

    fn build_pool(db_file: &Path) -> anyhow::Result<Pool<SqliteConnectionManager>> {
        let db_uri = format!("file:{}?mode=ro&immutable=1", db_file.to_string_lossy());
        let manager = SqliteConnectionManager::file(db_uri)
            .with_flags(
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_URI,
            )
            .with_init(|conn| {
                let _ = conn.pragma_update(None, "cache_size", "-64000");
                let _ = conn.pragma_update(None, "temp_store", "MEMORY");
                Ok(())
            });

        Pool::builder()
            .max_size(10)
            .min_idle(Some(2))
            .build(manager)
            .map_err(|e| {
                anyhow::anyhow!("Failed to create connection pool for {:?}: {}", db_file, e)
            })
    }

    pub fn get_db_connection(
        &self,
        dict_file: &Path,
    ) -> anyhow::Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        let dict_file = dict_file.to_path_buf();
        if let Some(pool) = self
            .runtime
            .db_pools
            .read()
            .expect("db_pools rwlock poisoned")
            .get(&dict_file)
            .cloned()
        {
            return pool
                .get()
                .map_err(|e| anyhow::anyhow!("Failed to get connection from pool: {}", e));
        }

        let db_file = Self::db_path(&dict_file);
        if !db_file.exists() {
            return Err(anyhow::anyhow!("Database file not found: {:?}", db_file));
        }

        let built_pool = Self::build_pool(&db_file)?;
        let pool = {
            let mut guard = self
                .runtime
                .db_pools
                .write()
                .expect("db_pools rwlock poisoned");
            guard.entry(dict_file).or_insert(built_pool).clone()
        };

        pool.get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection from pool: {}", e))
    }

    pub fn get_mdx_reader(&self, dict_file: &Path) -> anyhow::Result<Arc<MdxReader>> {
        let dict_file = dict_file.to_path_buf();
        if let Some(reader) = self
            .runtime
            .mdx_readers
            .read()
            .expect("mdx_readers rwlock poisoned")
            .get(&dict_file)
            .cloned()
        {
            return Ok(reader);
        }

        // P4: 按“当前词典总数”自适应 per-reader BlockCache 预算——词典多时
        // 均摊 `BLOCK_CACHE_TOTAL_BUDGET_BYTES`、词典少时保持原 64 MiB。
        // 字典在启动时一次扫描，后续不变，故以 dict_id_map 的词表状况为准。
        let budget = per_reader_cache_budget(self.catalog.dict_id_map.len().max(1));
        let reader = Arc::new(MdxReader::with_budget(&dict_file, budget)?);
        let entry = {
            let mut guard = self
                .runtime
                .mdx_readers
                .write()
                .expect("mdx_readers rwlock poisoned");
            guard.entry(dict_file).or_insert(reader).clone()
        };
        Ok(entry)
    }

    pub fn get_entry_cached(&self, key: &str) -> Option<(Bytes, Arc<str>)> {
        self.runtime
            .entry_cache
            .get(key)
            .map(|p| (p.data.clone(), p.content_type.clone()))
    }

    pub fn put_entry_cached(&self, key: String, data: Bytes, content_type: String) {
        self.runtime.entry_cache.insert(
            key,
            CachedPayload {
                data,
                content_type: Arc::from(content_type),
            },
        );
    }

    pub fn get_resource_cached(&self, key: &str) -> Option<(Bytes, Arc<str>)> {
        self.runtime
            .resource_cache
            .get(key)
            .map(|p| (p.data.clone(), p.content_type.clone()))
    }

    pub fn put_resource_cached(&self, key: String, data: Bytes, content_type: String) {
        self.runtime.resource_cache.insert(
            key,
            CachedPayload {
                data,
                content_type: Arc::from(content_type),
            },
        );
    }

    pub fn is_negative_cached(&self, key: &str) -> bool {
        self.runtime.negative_cache.contains_key(key)
    }

    pub fn put_negative_cache(&self, key: String) {
        self.runtime.negative_cache.insert(key, ());
    }

    pub fn clear_negative_cache(&self, key: &str) {
        self.runtime.negative_cache.invalidate(key);
    }

    pub fn try_acquire_query_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.runtime
            .blocking_query_slots
            .clone()
            .try_acquire_owned()
            .ok()
    }

    fn is_mdx_file(path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mdx"))
    }

    fn is_mdd_file(path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mdd"))
    }

    fn logical_dict_key(file: &Path) -> String {
        let file_stem = file.file_stem().unwrap_or_default();
        let parent = file.parent().unwrap_or(Path::new(""));
        parent
            .join(file_stem)
            .to_string_lossy()
            .to_string()
            .to_lowercase()
    }

    fn allocate_dict_id(logical_key: &str, id_to_logical: &mut HashMap<String, String>) -> String {
        let mut hasher = Ripemd160::new();
        hasher.update(logical_key.as_bytes());
        let base_id = format!("{:x}", hasher.finalize());

        let mut id = base_id.clone();
        let mut suffix: u32 = 1;
        while let Some(existing_key) = id_to_logical.get(&id) {
            if existing_key == logical_key {
                return id;
            }
            id = format!("{}-{}", base_id, suffix);
            suffix += 1;
        }

        id_to_logical.insert(id.clone(), logical_key.to_string());
        id
    }
}

#[cfg(test)]
mod tests {
    use super::AppState;
    use std::collections::HashMap;

    #[test]
    fn dict_id_allocation_is_stable_for_same_key() {
        let mut id_to_logical = HashMap::new();
        let id1 = AppState::allocate_dict_id("dict-key", &mut id_to_logical);
        let id2 = AppState::allocate_dict_id("dict-key", &mut id_to_logical);
        assert_eq!(id1, id2);
    }

    #[test]
    fn dict_id_allocation_adds_suffix_when_collision_detected() {
        let mut id_to_logical = HashMap::new();
        let base = AppState::allocate_dict_id("dict-key", &mut id_to_logical);

        id_to_logical.insert(base.clone(), "other-dict-key".to_string());
        let resolved = AppState::allocate_dict_id("dict-key", &mut id_to_logical);

        assert_ne!(resolved, base);
        assert!(resolved.starts_with(&(base + "-")));
    }
}
