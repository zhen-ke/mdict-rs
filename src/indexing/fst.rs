//! FST (Finite State Transducer) index builder for fuzzy search
//!
//! This module builds .fst index files from SQLite database entries.
//! FST provides memory-efficient fuzzy search with Levenshtein distance.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use anyhow::{Context, Result};
use fst::MapBuilder;
use rusqlite::Connection;
use tracing::info;

/// Build FST index from SQLite database
///
/// The FST maps lowercase words to a dummy value (we only need the keys for fuzzy matching).
/// Keys must be added in lexicographic order, so we query with ORDER BY.
pub fn build_fst_index(db_path: &str) -> Result<PathBuf> {
    let fst_path = PathBuf::from(format!("{}.fst", db_path.trim_end_matches(".db")));

    info!("Building FST index: {:?}", fst_path);

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database: {}", db_path))?;

    // Query all words, ordered by lowercase for FST requirement
    // Skip resource keys (starting with \ or /) and special entries
    let mut stmt = conn.prepare(
        "SELECT DISTINCT LOWER(text) as word FROM MDX_INDEX
         WHERE text NOT LIKE '\\%'
         AND text NOT LIKE '/%'
         AND text NOT LIKE '@%'
         AND LENGTH(text) > 0
         AND LENGTH(text) < 100
         ORDER BY word"
    )?;

    // Build FST
    let file = File::create(&fst_path)
        .with_context(|| format!("Failed to create FST file: {:?}", fst_path))?;
    let writer = BufWriter::new(file);
    let mut builder = MapBuilder::new(writer)
        .with_context(|| "Failed to create FST MapBuilder")?;

    // FST requires keys to be added in lexicographic order
    // Our SQL query already orders by lowercase word
    let mut prev_word: Option<String> = None;
    let mut added_count = 0u64;
    let mut row_count = 0u64;
    let mut rows = stmt.query([])?;

    while let Some(row) = rows.next()? {
        row_count += 1;
        let word: String = row.get(0)?;
        // Skip duplicates (case-insensitive)
        if prev_word.as_ref() == Some(&word) {
            continue;
        }

        // Skip empty or whitespace-only words
        let trimmed = word.trim();
        if trimmed.is_empty() {
            continue;
        }

        // FST keys must be strictly increasing
        if let Some(ref prev) = prev_word {
            if trimmed <= prev.as_str() {
                continue;
            }
        }

        builder.insert(trimmed, added_count)
            .with_context(|| format!("Failed to insert word: {}", trimmed))?;

        prev_word = Some(trimmed.to_string());
        added_count += 1;
    }

    builder.finish()
        .with_context(|| "Failed to finish FST build")?;

    info!("Scanned {} rows for FST index", row_count);
    info!("FST index built successfully: {} entries", added_count);

    Ok(fst_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fst_path_generation() {
        let db_path = "/path/to/dict.mdx.db";
        let expected = "/path/to/dict.mdx.fst";
        let result = format!("{}.fst", db_path.trim_end_matches(".db"));
        assert_eq!(result, expected);
    }
}
