use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use crate::config::{DictConfig, DictInfo};
use crate::mdict::reader::MdxReader;

const ENTRY_CACHE_SIZE: usize = 256;
const RESOURCE_CACHE_SIZE: usize = 1024;
const NEGATIVE_CACHE_SIZE: usize = 1024;
const ENTRY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const RESOURCE_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(20);

#[derive(Clone)]
struct CachedPayload {
    data: Vec<u8>,
    content_type: String,
    expires_at: Instant,
}

#[derive(Clone)]
pub struct AppState {
    dict_dir: Arc<PathBuf>,
    static_dir: Arc<PathBuf>,

    dict_text_files: Arc<Vec<PathBuf>>,
    dict_resource_files: Arc<Vec<PathBuf>>,

    dict_configs: Arc<HashMap<PathBuf, DictConfig>>,

    // Mapping between ID and File Path
    pub dict_id_map: Arc<HashMap<String, Vec<PathBuf>>>,
    pub path_to_id: Arc<HashMap<PathBuf, String>>,

    db_pools: Arc<Mutex<HashMap<PathBuf, Pool<SqliteConnectionManager>>>>,
    mdx_readers: Arc<Mutex<HashMap<PathBuf, Arc<MdxReader>>>>,
    entry_cache: Arc<Mutex<LruCache<String, CachedPayload>>>,
    resource_cache: Arc<Mutex<LruCache<String, CachedPayload>>>,
    negative_cache: Arc<Mutex<LruCache<String, Instant>>>,
}

impl AppState {
    pub fn new(dict_dir: PathBuf, static_dir: PathBuf, dict_files: Vec<PathBuf>) -> Self {
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

        let mut configs = HashMap::new();
        for file in &dict_text_files {
            if let Some(cfg) = DictConfig::load(file) {
                configs.insert(file.clone(), cfg);
            }
        }

        // Initialize ID mappings
        let mut dict_id_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut path_to_id = HashMap::new();

        // Generate IDs for all files
        for file in &dict_files {
            // Use file stem + parent path to identify "logical dictionary"
            let file_stem = file.file_stem().unwrap_or_default();
            let parent = file.parent().unwrap_or(Path::new(""));
            let logical_path = parent.join(file_stem);

            let path_str = logical_path.to_string_lossy().to_string().to_lowercase();
            let hash = adler32::adler32(path_str.as_bytes()).unwrap();
            let id = format!("{:x}", hash);

            dict_id_map
                .entry(id.clone())
                .or_default()
                .push(file.clone());
            path_to_id.insert(file.clone(), id);
        }

        Self {
            dict_dir: Arc::new(dict_dir),
            static_dir: Arc::new(static_dir),
            dict_text_files: Arc::new(dict_text_files),
            dict_resource_files: Arc::new(dict_resource_files),
            dict_configs: Arc::new(configs),
            dict_id_map: Arc::new(dict_id_map),
            path_to_id: Arc::new(path_to_id),
            db_pools: Arc::new(Mutex::new(HashMap::new())),
            mdx_readers: Arc::new(Mutex::new(HashMap::new())),
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
        let path = PathBuf::from(dict_id);
        self.dict_configs.get(&path).cloned()
    }

    pub fn get_all_dict_info(&self) -> Vec<DictInfo> {
        self.dict_text_files
            .iter()
            .map(|file| {
                let default_config = DictConfig::default();
                let cfg = self.dict_configs.get(file).unwrap_or(&default_config);
                DictInfo {
                    id: file.to_string_lossy().to_string(),
                    name: cfg.get_display_name(file),
                    description: cfg.description.clone(),
                    container_class: cfg.container_class.clone(),
                    has_css: cfg.css.is_some(),
                    has_js: cfg.js.is_some(),
                }
            })
            .collect()
    }

    pub fn get_dict_display_name(&self, dict_path: &Path) -> String {
        let default_config = DictConfig::default();
        self.dict_configs
            .get(dict_path)
            .unwrap_or(&default_config)
            .get_display_name(dict_path)
    }

    pub fn get_dict_container_class(&self, dict_path: &Path) -> Option<String> {
        self.dict_configs
            .get(dict_path)
            .and_then(|cfg| cfg.container_class.clone())
    }

    fn db_path(dict_file: &Path) -> PathBuf {
        PathBuf::from(format!("{}.db", dict_file.to_string_lossy()))
    }

    fn build_pool(db_file: &Path) -> anyhow::Result<Pool<SqliteConnectionManager>> {
        let manager = SqliteConnectionManager::file(db_file).with_init(|conn| {
            // SQLite 性能优化 (忽略错误，避免 panic)
            let _ = conn.pragma_update(None, "busy_timeout", "5000");
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
            let _ = conn.pragma_update(None, "synchronous", "NORMAL");
            let _ = conn.pragma_update(None, "cache_size", "-64000");
            let _ = conn.execute(
                "create index if not exists idx_mdx_text_nocase on MDX_INDEX(text COLLATE NOCASE)",
                [],
            );
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
            .lock()
            .expect("db_pools mutex poisoned")
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

        let pool = Self::build_pool(&db_file)?;
        let pool = {
            let mut guard = self.db_pools.lock().expect("db_pools mutex poisoned");
            guard.entry(dict_file).or_insert(pool).clone()
        };

        pool.get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection from pool: {}", e))
    }

    pub fn get_mdx_reader(&self, dict_file: &Path) -> anyhow::Result<Arc<MdxReader>> {
        let dict_file = dict_file.to_path_buf();
        if let Some(reader) = self
            .mdx_readers
            .lock()
            .expect("mdx_readers mutex poisoned")
            .get(&dict_file)
            .cloned()
        {
            return Ok(reader);
        }

        let reader = Arc::new(MdxReader::new(&dict_file)?);
        let mut map = self.mdx_readers.lock().expect("mdx_readers mutex poisoned");
        let entry = map.entry(dict_file).or_insert_with(|| reader.clone());
        Ok(entry.clone())
    }

    pub fn get_entry_cached(&self, key: &str) -> Option<(Vec<u8>, String)> {
        Self::cache_get(&self.entry_cache, key)
    }

    pub fn put_entry_cached(&self, key: String, data: Vec<u8>, content_type: String) {
        Self::cache_put(&self.entry_cache, key, data, content_type, ENTRY_CACHE_TTL);
    }

    pub fn get_resource_cached(&self, key: &str) -> Option<(Vec<u8>, String)> {
        Self::cache_get(&self.resource_cache, key)
    }

    pub fn put_resource_cached(&self, key: String, data: Vec<u8>, content_type: String) {
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

    fn is_mdx_file(path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mdx"))
    }

    fn is_mdd_file(path: &Path) -> bool {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mdd"))
    }

    fn cache_get(
        cache: &Arc<Mutex<LruCache<String, CachedPayload>>>,
        key: &str,
    ) -> Option<(Vec<u8>, String)> {
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
        data: Vec<u8>,
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
