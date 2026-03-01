use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use lru::LruCache;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use ripemd::{Digest, Ripemd160};
use rusqlite::OpenFlags;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::{DictConfig, DictInfo};
use crate::mdict::reader::MdxReader;

const ENTRY_CACHE_SIZE: usize = 256;
const RESOURCE_CACHE_SIZE: usize = 1024;
const NEGATIVE_CACHE_SIZE: usize = 1024;
const DEFAULT_MAX_CONCURRENT_BLOCKING_QUERIES: usize = 64;
const ENTRY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const RESOURCE_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(20);

#[derive(Clone)]
struct CachedPayload {
    data: Bytes,
    content_type: String,
    expires_at: Instant,
}

#[derive(Clone)]
pub struct AppState {
    dict_dir: Arc<PathBuf>,
    static_dir: Arc<PathBuf>,

    dict_text_files: Arc<Vec<PathBuf>>,
    dict_resource_files: Arc<Vec<PathBuf>>,

    dict_configs_by_path: Arc<HashMap<PathBuf, DictConfig>>,
    dict_configs_by_id: Arc<HashMap<String, DictConfig>>,
    id_to_primary_text: Arc<HashMap<String, PathBuf>>,

    // Mapping between ID and File Path
    dict_id_map: Arc<HashMap<String, Vec<PathBuf>>>,
    path_to_id: Arc<HashMap<PathBuf, String>>,

    db_pools: Arc<RwLock<HashMap<PathBuf, Pool<SqliteConnectionManager>>>>,
    mdx_readers: Arc<RwLock<HashMap<PathBuf, Arc<MdxReader>>>>,
    blocking_query_slots: Arc<Semaphore>,
    entry_cache: Arc<Mutex<LruCache<String, CachedPayload>>>,
    resource_cache: Arc<Mutex<LruCache<String, CachedPayload>>>,
    negative_cache: Arc<Mutex<LruCache<String, Instant>>>,
}

impl AppState {
    pub fn new(dict_dir: PathBuf, static_dir: PathBuf, dict_files: Vec<PathBuf>) -> Self {
        let max_concurrent_blocking_queries =
            std::env::var("MDICT_MAX_CONCURRENT_BLOCKING_QUERIES")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_MAX_CONCURRENT_BLOCKING_QUERIES);

        let dict_text_files: Vec<PathBuf> = dict_files
            .iter()
            .filter(|p| Self::is_mdx_file(p))
            .cloned()
            .collect();

        let mut mdd = Vec::new();
        let mut mdx = Vec::new();
        for file in &dict_files {
            if Self::is_mdd_file(file) {
                mdd.push(file.clone());
            } else {
                mdx.push(file.clone());
            }
        }
        mdd.extend(mdx);
        let dict_resource_files = mdd;

        let mut configs_by_path = HashMap::new();
        for file in &dict_text_files {
            if let Some(cfg) = DictConfig::load(file) {
                configs_by_path.insert(file.clone(), cfg);
            }
        }

        // Initialize ID mappings
        let mut dict_id_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut path_to_id = HashMap::new();
        let mut id_to_logical = HashMap::new();

        // Generate IDs for all files
        for file in &dict_files {
            // Use file stem + parent path to identify "logical dictionary"
            let logical_key = Self::logical_dict_key(file);
            let id = Self::allocate_dict_id(&logical_key, &mut id_to_logical);

            dict_id_map
                .entry(id.clone())
                .or_default()
                .push(file.clone());
            path_to_id.insert(file.clone(), id);
        }

        let mut id_to_primary_text = HashMap::new();
        let mut configs_by_id = HashMap::new();
        for (id, files) in &dict_id_map {
            let mut text_files: Vec<PathBuf> = files
                .iter()
                .filter(|f| Self::is_mdx_file(f))
                .cloned()
                .collect();
            text_files.sort();

            if let Some(primary_text) = text_files.into_iter().next() {
                id_to_primary_text.insert(id.clone(), primary_text.clone());
                if let Some(cfg) = configs_by_path.get(&primary_text) {
                    configs_by_id.insert(id.clone(), cfg.clone());
                }
            }
        }

        Self {
            dict_dir: Arc::new(dict_dir),
            static_dir: Arc::new(static_dir),
            dict_text_files: Arc::new(dict_text_files),
            dict_resource_files: Arc::new(dict_resource_files),
            dict_configs_by_path: Arc::new(configs_by_path),
            dict_configs_by_id: Arc::new(configs_by_id),
            id_to_primary_text: Arc::new(id_to_primary_text),
            dict_id_map: Arc::new(dict_id_map),
            path_to_id: Arc::new(path_to_id),
            db_pools: Arc::new(RwLock::new(HashMap::new())),
            mdx_readers: Arc::new(RwLock::new(HashMap::new())),
            blocking_query_slots: Arc::new(Semaphore::new(max_concurrent_blocking_queries)),
            entry_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(ENTRY_CACHE_SIZE).expect("ENTRY_CACHE_SIZE > 0"),
            ))),
            resource_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(RESOURCE_CACHE_SIZE).expect("RESOURCE_CACHE_SIZE > 0"),
            ))),
            negative_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(NEGATIVE_CACHE_SIZE).expect("NEGATIVE_CACHE_SIZE > 0"),
            ))),
        }
    }

    pub fn dict_dir(&self) -> &Path {
        &self.dict_dir
    }

    pub fn static_dir(&self) -> &Path {
        &self.static_dir
    }

    pub fn dict_text_files(&self) -> &[PathBuf] {
        &self.dict_text_files
    }

    pub fn dict_resource_files(&self) -> &[PathBuf] {
        &self.dict_resource_files
    }

    pub fn get_dict_id(&self, path: &Path) -> Option<String> {
        self.path_to_id.get(path).cloned()
    }

    pub fn get_dict_files(&self, id: &str) -> Option<&Vec<PathBuf>> {
        self.dict_id_map.get(id)
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
        if let Some(cfg) = self.dict_configs_by_id.get(dict_id) {
            return Some(cfg.clone());
        }

        // Backward compatibility for older clients that still send file paths as id.
        let path = PathBuf::from(dict_id);
        self.dict_configs_by_path.get(&path).cloned()
    }

    pub fn get_all_dict_info(&self) -> Vec<DictInfo> {
        let mut ids: Vec<&String> = self.id_to_primary_text.keys().collect();
        ids.sort();

        ids.into_iter()
            .filter_map(|id| {
                let file = self.id_to_primary_text.get(id)?;
                let default_config = DictConfig::default();
                let cfg = self
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
                })
            })
            .collect()
    }

    pub fn get_dict_display_name(&self, dict_path: &Path) -> String {
        let default_config = DictConfig::default();
        self.dict_configs_by_path
            .get(dict_path)
            .unwrap_or(&default_config)
            .get_display_name(dict_path)
    }

    pub fn get_dict_container_class(&self, dict_path: &Path) -> Option<String> {
        self.dict_configs_by_path
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
                // Read-path tuning only: avoid any DDL/pragma that requires writes.
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
            let mut guard = self.db_pools.write().expect("db_pools rwlock poisoned");
            guard.entry(dict_file).or_insert(built_pool).clone()
        };

        pool.get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection from pool: {}", e))
    }

    pub fn get_mdx_reader(&self, dict_file: &Path) -> anyhow::Result<Arc<MdxReader>> {
        let dict_file = dict_file.to_path_buf();
        if let Some(reader) = self
            .mdx_readers
            .read()
            .expect("mdx_readers rwlock poisoned")
            .get(&dict_file)
            .cloned()
        {
            return Ok(reader);
        }

        let reader = Arc::new(MdxReader::new(&dict_file)?);
        let entry = {
            let mut guard = self
                .mdx_readers
                .write()
                .expect("mdx_readers rwlock poisoned");
            guard.entry(dict_file).or_insert(reader).clone()
        };
        Ok(entry)
    }

    pub fn get_entry_cached(&self, key: &str) -> Option<(Bytes, String)> {
        Self::cache_get(&self.entry_cache, key)
    }

    pub fn put_entry_cached(&self, key: String, data: Bytes, content_type: String) {
        Self::cache_put(&self.entry_cache, key, data, content_type, ENTRY_CACHE_TTL);
    }

    pub fn get_resource_cached(&self, key: &str) -> Option<(Bytes, String)> {
        Self::cache_get(&self.resource_cache, key)
    }

    pub fn put_resource_cached(&self, key: String, data: Bytes, content_type: String) {
        Self::cache_put(
            &self.resource_cache,
            key,
            data,
            content_type,
            RESOURCE_CACHE_TTL,
        );
    }

    pub fn is_negative_cached(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut cache = self
            .negative_cache
            .lock()
            .expect("negative_cache mutex poisoned");
        if let Some(expiry) = cache.get(key).cloned() {
            if expiry > now {
                return true;
            }
            cache.pop(key);
        }
        false
    }

    pub fn put_negative_cache(&self, key: String) {
        let expiry = Instant::now() + NEGATIVE_CACHE_TTL;
        self.negative_cache
            .lock()
            .expect("negative_cache mutex poisoned")
            .put(key, expiry);
    }

    pub fn clear_negative_cache(&self, key: &str) {
        self.negative_cache
            .lock()
            .expect("negative_cache mutex poisoned")
            .pop(key);
    }

    pub fn try_acquire_query_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.blocking_query_slots.clone().try_acquire_owned().ok()
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

    fn cache_get(
        cache: &Arc<Mutex<LruCache<String, CachedPayload>>>,
        key: &str,
    ) -> Option<(Bytes, String)> {
        let now = Instant::now();
        let mut cache = cache.lock().expect("cache mutex poisoned");
        if let Some(payload) = cache.get(key).cloned() {
            if payload.expires_at > now {
                return Some((payload.data, payload.content_type));
            }
            cache.pop(key);
        }
        None
    }

    fn cache_put(
        cache: &Arc<Mutex<LruCache<String, CachedPayload>>>,
        key: String,
        data: Bytes,
        content_type: String,
        ttl: Duration,
    ) {
        let payload = CachedPayload {
            data,
            content_type,
            expires_at: Instant::now() + ttl,
        };
        cache
            .lock()
            .expect("cache mutex poisoned")
            .put(key, payload);
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

        // Simulate a rare hash collision on the base ID.
        id_to_logical.insert(base.clone(), "other-dict-key".to_string());
        let resolved = AppState::allocate_dict_id("dict-key", &mut id_to_logical);

        assert_ne!(resolved, base);
        assert!(resolved.starts_with(&(base + "-")));
    }
}
