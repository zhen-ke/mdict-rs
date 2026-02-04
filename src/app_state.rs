use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use crate::config::{DictConfig, DictInfo};
use crate::mdict::reader::MdxReader;

#[derive(Clone)]
pub struct AppState {
    dict_dir: Arc<PathBuf>,
    static_dir: Arc<PathBuf>,

    dict_text_files: Arc<Vec<PathBuf>>,
    dict_resource_files: Arc<Vec<PathBuf>>,

    dict_configs: Arc<HashMap<PathBuf, DictConfig>>,

    db_pools: Arc<Mutex<HashMap<PathBuf, Pool<SqliteConnectionManager>>>>,
    mdx_readers: Arc<Mutex<HashMap<PathBuf, Arc<MdxReader>>>>,
}

impl AppState {
    pub fn new(dict_dir: PathBuf, static_dir: PathBuf, dict_files: Vec<PathBuf>) -> Self {
        let dict_text_files: Vec<PathBuf> = dict_files
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mdx")))
            .cloned()
            .collect();

        let mut mdd = Vec::new();
        let mut mdx = Vec::new();
        for file in &dict_files {
            if file
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("mdd"))
            {
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

        Self {
            dict_dir: Arc::new(dict_dir),
            static_dir: Arc::new(static_dir),
            dict_text_files: Arc::new(dict_text_files),
            dict_resource_files: Arc::new(dict_resource_files),
            dict_configs: Arc::new(configs),
            db_pools: Arc::new(Mutex::new(HashMap::new())),
            mdx_readers: Arc::new(Mutex::new(HashMap::new())),
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
}
