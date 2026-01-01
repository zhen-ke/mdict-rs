use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::fs;

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
pub static DB_POOLS: LazyLock<HashMap<String, Pool<SqliteConnectionManager>>> =
    LazyLock::new(|| {
        info!("Initializing database pools...");
        let mut pools = HashMap::new();

        if MDX_FILES.is_empty() {
            warn!("No dictionary files to index, DB_POOLS will be empty");
            return pools;
        }

        for file in MDX_FILES.iter() {
            let db_file = format!("{file}.db");

            // 检查 db 文件是否存在
            if !std::path::Path::new(&db_file).exists() {
                warn!("Database file not found: {}, waiting for indexing...", db_file);
                continue;
            }

            let manager = SqliteConnectionManager::file(&db_file).with_init(|conn| {
                // SQLite 性能优化 (使用 ok() 忽略错误，避免 panic)
                let _ = conn.pragma_update(None, "busy_timeout", "5000");
                let _ = conn.pragma_update(None, "journal_mode", "WAL");
                let _ = conn.pragma_update(None, "synchronous", "NORMAL");
                let _ = conn.pragma_update(None, "cache_size", "-64000");
                Ok(())
            });

            match Pool::builder()
                .max_size(10)
                .min_idle(Some(2))
                .build(manager) {
                Ok(pool) => {
                    pools.insert(file.to_string(), pool);
                }
                Err(e) => {
                    error!("Failed to create connection pool for {}: {}", db_file, e);
                }
            }
        }

        pools
    });

/// 从连接池获取数据库连接
pub fn get_db_connection(
    file: &str,
) -> anyhow::Result<r2d2::PooledConnection<SqliteConnectionManager>> {
    info!("get connection from pool...");
    let pools = &*DB_POOLS;
    let pool = pools
        .get(file)
        .ok_or_else(|| anyhow::anyhow!("No connection pool found for file: {}", file))?;

    pool.get()
        .map_err(|e| anyhow::anyhow!("Failed to get connection from pool: {}", e))
}

/// MDX 读取器缓存
pub static MDX_RESOURCES: LazyLock<HashMap<String, MdxReader>> = LazyLock::new(|| {
    info!("Initializing MDX readers...");
    let mut map = HashMap::new();

    if MDX_FILES.is_empty() {
        warn!("No dictionary files found, MDX_RESOURCES will be empty");
        return map;
    }

    for file in MDX_FILES.iter() {
        match MdxReader::new(file) {
            Ok(reader) => {
                map.insert(file.to_string(), reader);
            }
            Err(e) => {
                error!("Failed to create reader for {}: {}", file, e);
            }
        }
    }
    map
});

pub fn get_mdx_reader(file: &str) -> anyhow::Result<&MdxReader> {
    MDX_RESOURCES
        .get(file)
        .ok_or_else(|| anyhow::anyhow!("No reader found for file: {}", file))
}

/// FST 索引缓存 - 使用 memmap 懒加载，节省内存
pub static FST_INDEXES: LazyLock<HashMap<String, fst::Map<memmap2::Mmap>>> = LazyLock::new(|| {
    info!("Initializing FST indexes...");
    let mut map = HashMap::new();

    if MDX_FILES.is_empty() {
        warn!("No dictionary files found, FST_INDEXES will be empty");
        return map;
    }

    for file in MDX_FILES.iter() {
        // Skip .mdd files (they don't have text entries)
        if file.ends_with(".mdd") {
            continue;
        }

        let fst_path = format!("{}.fst", file);
        if !std::path::Path::new(&fst_path).exists() {
            warn!("FST file not found: {}, fuzzy search disabled for this dict", fst_path);
            continue;
        }

        match load_fst_index(&fst_path) {
            Ok(fst_map) => {
                info!("Loaded FST index: {} ({} entries)", fst_path, fst_map.len());
                map.insert(file.to_string(), fst_map);
            }
            Err(e) => {
                error!("Failed to load FST index {}: {}", fst_path, e);
            }
        }
    }

    map
});

/// 加载 FST 索引文件
fn load_fst_index(fst_path: &str) -> anyhow::Result<fst::Map<memmap2::Mmap>> {
    let file = fs::File::open(fst_path)?;
    let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };
    let fst_map = fst::Map::new(mmap)?;
    Ok(fst_map)
}

/// 获取 FST 索引
pub fn get_fst_index(file: &str) -> Option<&fst::Map<memmap2::Mmap>> {
    FST_INDEXES.get(file)
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
    let dict_dir = get_dict_dir();

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
