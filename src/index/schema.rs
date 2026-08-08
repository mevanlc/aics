use anyhow::{Context, Result};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, STORED,
    STRING,
};
use tantivy::tokenizer::{LowerCaser, RawTokenizer, TextAnalyzer};
use tantivy::Index;

const CONTENT_FIELD: &str = "content";
const WORKING_DIR_FIELD: &str = "working_dir";
const WORKING_DIR_TOKENIZER: &str = "working_dir";
const FILE_PATH_FIELD: &str = "file_path";
const MODIFIED_TS_FIELD: &str = "modified_ts";
const SESSION_JSON_FIELD: &str = "session_json";

#[derive(Debug, Clone)]
pub struct IndexSchema {
    pub schema: Schema,
    pub content: Field,
    pub working_dir: Field,
    pub file_path: Field,
    pub modified_ts: Field,
    pub session_json: Field,
}

impl Default for IndexSchema {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexSchema {
    pub fn new() -> Self {
        let mut builder = Schema::builder();
        let content_options = TextOptions::default().set_stored().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("default")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        let working_dir_options = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(WORKING_DIR_TOKENIZER)
                .set_index_option(IndexRecordOption::Basic),
        );
        let stored_text = TextOptions::default().set_stored();
        let numeric_options = NumericOptions::default()
            .set_fast()
            .set_stored()
            .set_indexed();

        let content = builder.add_text_field(CONTENT_FIELD, content_options);
        let working_dir = builder.add_text_field(WORKING_DIR_FIELD, working_dir_options);
        let file_path = builder.add_text_field(FILE_PATH_FIELD, STRING | STORED);
        let modified_ts = builder.add_u64_field(MODIFIED_TS_FIELD, numeric_options);
        let session_json = builder.add_text_field(SESSION_JSON_FIELD, stored_text);

        Self {
            schema: builder.build(),
            content,
            working_dir,
            file_path,
            modified_ts,
            session_json,
        }
    }

    pub fn register_tokenizers(index: &Index) {
        index.tokenizers().register(
            WORKING_DIR_TOKENIZER,
            TextAnalyzer::builder(RawTokenizer::default())
                .filter(LowerCaser)
                .build(),
        );
    }

    pub fn from_schema(schema: &Schema) -> Result<Self> {
        Ok(Self {
            content: schema
                .get_field(CONTENT_FIELD)
                .context("missing content field")?,
            working_dir: schema
                .get_field(WORKING_DIR_FIELD)
                .context("missing working_dir field")?,
            file_path: schema
                .get_field(FILE_PATH_FIELD)
                .context("missing file_path field")?,
            modified_ts: schema
                .get_field(MODIFIED_TS_FIELD)
                .context("missing modified_ts field")?,
            session_json: schema
                .get_field(SESSION_JSON_FIELD)
                .context("missing session_json field")?,
            schema: schema.clone(),
        })
    }
}
