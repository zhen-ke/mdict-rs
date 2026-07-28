use std::iter::Iterator;

use rand::{Rng, rng};
use rusqlite::Connection;
use std::sync::LazyLock;
use tracing::debug;

use crate::app_state::AppState;

/// Pick a random real word from the loaded dictionaries' SQLite indexes.
///
/// Strategy:
///   1. Randomly pick one text-dictionary that has a ready DB.
///   2. Run `SELECT text FROM MDX_INDEX WHERE id = :rand_id` using a random row id
///      within `[1, max_id]`.  Retry up to 5 times to skip invalid/resource entries.
///   3. If all attempts fail (DB not ready, empty table, etc.), fall back to the
///      built-in hardcoded word list so the feature never errors out.
pub fn lucky_word(state: &AppState) -> String {
    if let Some(word) = pick_from_db(state) {
        return word;
    }
    // Fallback to hardcoded list (e.g. DB not ready during startup indexing)
    debug!("lucky: falling back to hardcoded word list");
    let random_index = rng().random_range(0..WORD_LIST.len());
    WORD_LIST[random_index].to_string()
}

/// Attempt to pick a random word from a real dictionary index.
fn pick_from_db(state: &AppState) -> Option<String> {
    let text_files = state.dict_text_files();
    if text_files.is_empty() {
        return None;
    }

    // Shuffle dictionary order so we don't always hit the same one
    let start = rng().random_range(0..text_files.len());
    for i in 0..text_files.len() {
        let file = &text_files[(start + i) % text_files.len()];
        let conn = match state.get_db_connection(file) {
            Ok(c) => c,
            Err(_) => continue, // DB not ready yet
        };
        if let Some(word) = pick_random_word(&conn) {
            return Some(word);
        }
    }
    None
}

/// Pick a random valid word from a single dictionary's MDX_INDEX table.
///
/// Uses `SELECT text FROM MDX_INDEX WHERE id >= :rand_id ORDER BY id LIMIT 1`
/// with a random id in `[1, max_id]`.  This avoids `ORDER BY RANDOM()` which
/// would scan the entire table on every call.
fn pick_random_word(conn: &Connection) -> Option<String> {
    let max_id: i64 = conn
        .query_row("SELECT MAX(id) FROM MDX_INDEX", [], |row| row.get(0))
        .ok()?;
    if max_id <= 0 {
        return None;
    }

    const MAX_ATTEMPTS: usize = 5;
    for _ in 0..MAX_ATTEMPTS {
        let rand_id: i64 = rng().random_range(1..=max_id);
        // `>= :id ORDER BY id LIMIT 1` guarantees O(1) index seek even if
        // some ids are missing (they aren't, but this is safer).
        let result: Result<String, _> = conn.query_row(
            "SELECT text FROM MDX_INDEX WHERE id >= ?1 ORDER BY id LIMIT 1",
            [rand_id],
            |row| row.get(0),
        );
        if let Ok(word) = result
            && is_displayable_word(&word) {
                return Some(word);
            }
    }
    None
}

/// Filter out entries that aren't suitable for "I'm feeling lucky" display.
///
/// Rejects resource paths, internal markers, overly long entries,
/// and other non-word content that would look wrong as a random discovery.
fn is_displayable_word(word: &str) -> bool {
    if word.is_empty() || word.len() > 80 {
        return false;
    }
    // Resource paths (images, css, etc.)
    if word.starts_with('\\') || word.starts_with('/') {
        return false;
    }
    // Internal markers
    if word.starts_with('@') || word.starts_with('.') {
        return false;
    }
    // HTML fragments or tags
    if word.contains('<') || word.contains('>') {
        return false;
    }
    true
}

static WORD_LIST: LazyLock<Vec<&str>> = LazyLock::new(|| STRING_LINES.lines().collect());
static STRING_LINES: &str = r#"abjure
abrogate
abstemious
acumen
antebellum
auspicious
belie
bellicose
bowdlerize
chicanery
chromosome
churlish
circumlocution
circumnavigate
deciduous
deleterious
diffident
enervate
enfranchise
epiphany
equinox
euro
evanescent
expurgate
facetious
fatuous
feckless
fiduciary
filibuster
gamete
gauche
gerrymander
hegemony
hemoglobin
homogeneous
hubris
hypotenuse
impeach
incognito
incontrovertible
inculcate
infrastructure
interpolate
irony
jejune
kinetic
kowtow
laissez faire
lexicon
loquacious
lugubrious
metamorphosis
mitosis
moiety
nanotechnology
nihilism
nomenclature
nonsectarian
notarize
obsequious
oligarchy
omnipotent
orthography
oxidize
parabola
paradigm
parameter
pecuniary
photosynthesis
plagiarize
plasma
polymer
precipitous
quasar
quotidian
recapitulate
reciprocal
reparation
respiration
sanguine
soliloquy
subjugate
suffragist
supercilious
tautology
taxonomy
tectonic
tempestuous
thermodynamics
totalitarian
unctuous
usurp
vacuous
vehement
vortex
winnow
wrought
xenophobe
yeoman
ziggurat
salient"#;
