//! 生词本（收藏）存储：SQLite 单文件 `favorites.db`（位于词典目录）。
//!
//! 数据量小、操作简单，用一把 Mutex 保护连接即可，无需连接池。表结构：
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS favorites (
//!     word     TEXT PRIMARY KEY,   -- 收藏词（原文，大小写敏感）
//!     added_at INTEGER NOT NULL    -- 收藏时间（unix 秒）
//! );
//! ```

use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Mutex;

/// 收藏存储。所有 `AppState` 克隆共享同一实例（`Arc`）。
pub struct FavoritesStore {
    conn: Mutex<Connection>,
}

impl FavoritesStore {
    /// 打开（必要时创建）收藏数据库。返回 `None` 表示打开失败
    /// （如目录只读），调用方应降级为"收藏不可用"而不是崩溃。
    pub fn open(dict_dir: &Path) -> Option<Self> {
        let db_path = dict_dir.join("favorites.db");
        let conn = Connection::open(&db_path).ok()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS favorites (
                word     TEXT PRIMARY KEY,
                added_at INTEGER NOT NULL
            );",
        )
        .ok()?;
        Some(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 全部收藏词（按收藏时间倒序）。
    pub fn list(&self) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT word FROM favorites ORDER BY added_at DESC, word ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// 添加收藏；已存在时更新时间戳（幂等）。
    pub fn add(&self, word: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO favorites (word, added_at) VALUES (?1, ?2)
             ON CONFLICT(word) DO UPDATE SET added_at = excluded.added_at",
            params![word.trim(), unix_now()],
        )?;
        Ok(())
    }

    /// 移除单个收藏。返回是否真的存在。
    pub fn remove(&self, word: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let affected = conn.execute(
            "DELETE FROM favorites WHERE word = ?1",
            params![word.trim()],
        )?;
        Ok(affected > 0)
    }

    /// 清空全部收藏。
    pub fn clear(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM favorites", [])?;
        Ok(())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
