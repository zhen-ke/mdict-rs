mod error;
mod repository;
mod service;
mod specific;

pub use error::QueryError;
pub use service::{DictFilter, query, query_aggregate, query_with_trace, suggest};
pub use specific::{query_specific_entry, query_specific_resource};

pub(crate) use mdict_core::normalize::{canonical_normalize, entry_query_candidates};
pub(crate) use repository::{
    EntryCandidateLookup, MAX_RESOURCE_RECORD_BYTES, detect_content_type, lookup_entry_candidate,
    lookup_entry_candidate_normalized, lookup_record_in_file, rewrite_entry_html_record,
};
