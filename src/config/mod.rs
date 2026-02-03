use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use tracing::{info, error, warn};
use crate::mdict::reader::MdxReader;

mod dict_config;
pub use dict_config::{DictConfig, DictInfo};

/// 获取词典目录路径
/// 优先级:
/// 1. 环境变量 MDX_DICT_DIR
/// 2. 二进制同级的 mdict 文件夹
/// 3. 当前工作目录的 mdict 文件夹
fn get_dict_dir() -> PathBuf {
    // 1. 尝试环境变量
    if let Ok(dir) = env::var("MDX_DICT_DIR") {
        let path = PathBuf::from(&dir);
        if path.exists() {
            info!("Using dict dir from MDX_DICT_DIR: {}", dir);
            return path;
        }
        warn!("MDX_DICT_DIR={} does not exist, falling back", dir);
    }

    // 2. 尝试二进制同级目录
    if let Ok(exe_path) = env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let dict_dir = exe_dir.join("mdict");
            if dict_dir.exists() {
                info!("Using dict dir next to binary: {:?}", dict_dir);
                return dict_dir;
            }
        }
    }

    // 3. 当前工作目录
    let cwd_dict = PathBuf::from("mdict");
    if cwd_dict.exists() {
        info!("Using dict dir in current directory: {:?}", cwd_dict);
        return cwd_dict;
    }

    // 默认返回当前目录的 mdict (可能不存在)
    warn!("No dict directory found, using ./mdict");
    cwd_dict
}

/// 扫描词典目录，返回所有 .mdx 和 .mdd 文件路径
fn scan_dict_files() -> Vec<String> {
    let dict_dir = get_dict_dir();
    let mut files = Vec::new();

    if !dict_dir.exists() {
        error!("Dict directory does not exist: {:?}", dict_dir);
        error!("Please create the 'mdict' folder and add .mdx/.mdd files");
        return files;
    }

    info!("Scanning dict directory: {:?}", dict_dir);

    if let Ok(entries) = fs::read_dir(&dict_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // 跳过 macOS 元数据文件 (._*)
            if let Some(file_name) = path.file_name() {
                let name = file_name.to_string_lossy();
                if name.starts_with("._") {
                    continue;
                }
            }

            if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                if ext_str == "mdx" || ext_str == "mdd" {
                    let path_str = path.to_string_lossy().to_string();
                    info!("Found dict file: {}", path_str);
                    files.push(path_str);
                }
            }
        }
    }

    // 排序：.mdx 文件在前，.mdd 文件在后
    files.sort_by(|a, b| {
        let a_is_mdx = a.ends_with(".mdx");
        let b_is_mdx = b.ends_with(".mdx");
        match (a_is_mdx, b_is_mdx) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.cmp(b),
        }
    });

    if files.is_empty() {
        error!("========================================");
        error!("ERROR: No dictionary files found!");
        error!("Please add .mdx/.mdd files to: {:?}", dict_dir);
        error!("========================================");
        // 不要 panic，返回空列表让程序继续运行
    } else {
        info!("Found {} dict files:", files.len());
        for f in &files {
            info!("  - {}", f);
        }
    }

    files
}

/// 动态扫描的词典文件列表
pub static MDX_FILES: LazyLock<Vec<String>> = LazyLock::new(scan_dict_files);

/// 仅包含 .mdx 的词典（文本查询）
pub static MDX_TEXT_FILES: LazyLock<Vec<String>> = LazyLock::new(|| {
    MDX_FILES
        .iter()
        .filter(|f| f.ends_with(".mdx"))
        .cloned()
        .collect()
});

/// 资源优先：.mdd 在前，.mdx 在后（资源查询）
pub static MDX_RESOURCE_FILES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut mdd = Vec::new();
    let mut mdx = Vec::new();
    for file in MDX_FILES.iter() {
        if file.ends_with(".mdd") {
            mdd.push(file.clone());
        } else {
            mdx.push(file.clone());
        }
    }
    mdd.extend(mdx);
    mdd
});

/// 获取静态资源路径
/// 优先级:
/// 1. 二进制同级的 static 文件夹
/// 2. 当前工作目录的 static 文件夹
/// 3. 开发模式的 resources/static
pub fn static_path() -> anyhow::Result<PathBuf> {
    // 1. 二进制同级
    if let Ok(exe_path) = env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let static_dir = exe_dir.join("static");
            if static_dir.exists() {
                return Ok(static_dir);
            }
        }
    }

    // 2. 当前目录
    let cwd_static = PathBuf::from("static");
    if cwd_static.exists() {
        return Ok(cwd_static);
    }

    // 3. 开发模式
    let mut dev_path: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    dev_path.push("resources/static");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    Err(anyhow::anyhow!("Static directory not found"))
}

/// 全局数据库连接池，为每个MDX文件维护一个连接池
pub static DB_POOLS: LazyLock<Mutex<HashMap<String, Pool<SqliteConnectionManager>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn build_pool(db_file: &str) -> anyhow::Result<Pool<SqliteConnectionManager>> {
    let manager = SqliteConnectionManager::file(db_file).with_init(|conn| {
        // SQLite 性能优化 (使用 ok() 忽略错误，避免 panic)
        let _ = conn.pragma_update(None, "busy_timeout", "5000");
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        let _ = conn.pragma_update(None, "cache_size", "-64000");
        // Ensure prefix search can use NOCASE index when table exists
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
        .map_err(|e| anyhow::anyhow!("Failed to create connection pool for {}: {}", db_file, e))
}

/// 从连接池获取数据库连接
pub fn get_db_connection(
    file: &str,
) -> anyhow::Result<r2d2::PooledConnection<SqliteConnectionManager>> {
    if let Some(pool) = DB_POOLS
        .lock()
        .expect("DB_POOLS mutex poisoned")
        .get(file)
        .cloned()
    {
        return pool
            .get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection from pool: {}", e));
    }

    let db_file = format!("{file}.db");
    if !Path::new(&db_file).exists() {
        return Err(anyhow::anyhow!("Database file not found: {}", db_file));
    }

    let pool = build_pool(&db_file)?;
    let pool = {
        let mut guard = DB_POOLS
            .lock()
            .expect("DB_POOLS mutex poisoned");
        guard.entry(file.to_string()).or_insert(pool).clone()
    };

    pool.get()
        .map_err(|e| anyhow::anyhow!("Failed to get connection from pool: {}", e))
}

/// MDX 读取器缓存
pub static MDX_RESOURCES: LazyLock<Mutex<HashMap<String, Arc<MdxReader>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn get_mdx_reader(file: &str) -> anyhow::Result<Arc<MdxReader>> {
    if let Some(reader) = MDX_RESOURCES
        .lock()
        .expect("MDX_RESOURCES mutex poisoned")
        .get(file)
        .cloned()
    {
        return Ok(reader);
    }

    let reader = Arc::new(MdxReader::new(file)?);
    let mut map = MDX_RESOURCES
        .lock()
        .expect("MDX_RESOURCES mutex poisoned");
    let entry = map.entry(file.to_string()).or_insert_with(|| reader.clone());
    Ok(entry.clone())
}

/// 词典配置缓存
pub static DICT_CONFIGS: LazyLock<HashMap<String, DictConfig>> = LazyLock::new(|| {
    info!("Loading dictionary configs...");
    let mut configs = HashMap::new();

    for file in MDX_FILES.iter() {
        // Only load configs for .mdx files, not .mdd
        if file.ends_with(".mdd") {
            continue;
        }

        if let Some(config) = DictConfig::load(file) {
            info!("Loaded config for: {}", file);
            configs.insert(file.clone(), config);
        }
    }

    info!("Loaded {} dictionary configs", configs.len());
    configs
});

/// 获取词典配置
pub fn get_dict_config(mdx_file: &str) -> Option<&DictConfig> {
    DICT_CONFIGS.get(mdx_file)
}

/// 获取词典目录（公开版本）
pub fn get_dict_directory() -> PathBuf {
    get_dict_dir()
}

/// 获取所有词典信息（用于 API 响应）
pub fn get_all_dict_info() -> Vec<DictInfo> {
    MDX_FILES
        .iter()
        .filter(|f| f.ends_with(".mdx"))
        .map(|file| {
            let config = get_dict_config(file);
            let default_config = DictConfig::default();
            let cfg = config.unwrap_or(&default_config);

            DictInfo {
                id: file.clone(),
                name: cfg.get_display_name(file),
                description: cfg.description.clone(),
                container_class: cfg.container_class.clone(),
                has_css: cfg.css.is_some(),
                has_js: cfg.js.is_some(),
            }
        })
        .collect()
}
