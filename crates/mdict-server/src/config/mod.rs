use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

mod dict_config;
pub use dict_config::{DictConfig, DictInfo};

/// 获取词典目录路径
/// 优先级:
/// 1. 环境变量 MDX_DICT_DIR
/// 2. 二进制同级的 mdict 文件夹
/// 3. 当前工作目录的 mdict 文件夹
pub fn get_dict_dir() -> PathBuf {
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
pub fn scan_dict_files(dict_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if !dict_dir.exists() {
        error!("Dict directory does not exist: {:?}", dict_dir);
        error!("Please create the 'mdict' folder and add .mdx/.mdd files");
        return files;
    }

    info!("Scanning dict directory: {:?}", dict_dir);

    if let Ok(entries) = fs::read_dir(dict_dir) {
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
                    let canonical = path.canonicalize().unwrap_or(path);
                    info!("Found dict file: {:?}", canonical);
                    files.push(canonical);
                }
            }
        }
    }

    // 排序：.mdx 文件在前，.mdd 文件在后
    files.sort_by(|a, b| {
        let a_is_mdx = a.extension().is_some_and(|e| e.eq_ignore_ascii_case("mdx"));
        let b_is_mdx = b.extension().is_some_and(|e| e.eq_ignore_ascii_case("mdx"));
        match (a_is_mdx, b_is_mdx) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.to_string_lossy().cmp(&b.to_string_lossy()),
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
            info!("  - {:?}", f);
        }
    }

    files
}

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
