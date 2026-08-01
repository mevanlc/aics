pub mod reader;
pub mod schema;
pub mod writer;

pub use reader::{
    Scope, SearchEngine, SearchFilters, SearchHit, SearchRequest, SortMode, SupersededFilter,
    TrashFilter,
};
pub use writer::{IndexManager, IndexPaths, StoredSession, SyncOutcome, SyncProgress, SyncStats};
