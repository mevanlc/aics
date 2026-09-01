use anyhow::{Context, Result};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, STORED,
    STRING,
};
use tantivy::tokenizer::{LowerCaser, RawTokenizer, TextAnalyzer};
use tantivy::Index;

const CONTENT_FIELD: &str = "content";
const USER_FIELD: &str = "user";
const AGENT_FIELD: &str = "agent";
const TOOL_CALL_FIELD: &str = "toolcall";
const TOOL_RESULT_FIELD: &str = "toolresult";
const DIRS_FIELD: &str = "dirs";
const FILES_FIELD: &str = "files";
const PATHS_FIELD: &str = "paths";
const VIS_ALWAYS_FIELD: &str = "_vis_always";
const VIS_USER_FIELD: &str = "_vis_user";
const VIS_AGENT_FIELD: &str = "_vis_agent";
const VIS_TOOL_CALL_FIELD: &str = "_vis_toolcall";
const VIS_TOOL_RESULT_FIELD: &str = "_vis_toolresult";
const VIS_TOOL_CALL_RESULT_FIELD: &str = "_vis_toolcall_result";
const VIS_PROJECT_DOCS_FIELD: &str = "_vis_projectdocs";
const VIS_USER_PROJECT_DOCS_FIELD: &str = "_vis_user_projectdocs";
const VIS_USER_SKILL_FIELD: &str = "_vis_user_skill";
const WORKING_DIR_FIELD: &str = "working_dir";
const WORKING_DIR_TOKENIZER: &str = "working_dir";
const FILE_PATH_FIELD: &str = "file_path";
const MODIFIED_TS_FIELD: &str = "modified_ts";
const SESSION_JSON_FIELD: &str = "session_json";

#[derive(Debug, Clone)]
pub struct IndexSchema {
    pub schema: Schema,
    pub content: Field,
    pub user: Field,
    pub agent: Field,
    pub tool_call: Field,
    pub tool_result: Field,
    pub dirs: Field,
    pub files: Field,
    pub paths: Field,
    pub vis_always: Field,
    pub vis_user: Field,
    pub vis_agent: Field,
    pub vis_tool_call: Field,
    pub vis_tool_result: Field,
    pub vis_tool_call_result: Field,
    pub vis_project_docs: Field,
    pub vis_user_project_docs: Field,
    pub vis_user_skill: Field,
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
        let path_options = working_dir_options.clone().set_stored();
        let stored_text = TextOptions::default().set_stored();
        let numeric_options = NumericOptions::default()
            .set_fast()
            .set_stored()
            .set_indexed();

        let content = builder.add_text_field(CONTENT_FIELD, content_options.clone());
        let user = builder.add_text_field(USER_FIELD, content_options.clone());
        let agent = builder.add_text_field(AGENT_FIELD, content_options.clone());
        let tool_call = builder.add_text_field(TOOL_CALL_FIELD, content_options.clone());
        let tool_result = builder.add_text_field(TOOL_RESULT_FIELD, content_options.clone());
        let dirs = builder.add_text_field(DIRS_FIELD, path_options.clone());
        let files = builder.add_text_field(FILES_FIELD, path_options.clone());
        let paths = builder.add_text_field(PATHS_FIELD, path_options);
        let vis_always = builder.add_text_field(VIS_ALWAYS_FIELD, content_options.clone());
        let vis_user = builder.add_text_field(VIS_USER_FIELD, content_options.clone());
        let vis_agent = builder.add_text_field(VIS_AGENT_FIELD, content_options.clone());
        let vis_tool_call = builder.add_text_field(VIS_TOOL_CALL_FIELD, content_options.clone());
        let vis_tool_result =
            builder.add_text_field(VIS_TOOL_RESULT_FIELD, content_options.clone());
        let vis_tool_call_result =
            builder.add_text_field(VIS_TOOL_CALL_RESULT_FIELD, content_options.clone());
        let vis_project_docs =
            builder.add_text_field(VIS_PROJECT_DOCS_FIELD, content_options.clone());
        let vis_user_project_docs =
            builder.add_text_field(VIS_USER_PROJECT_DOCS_FIELD, content_options.clone());
        let vis_user_skill = builder.add_text_field(VIS_USER_SKILL_FIELD, content_options);
        let working_dir = builder.add_text_field(WORKING_DIR_FIELD, working_dir_options);
        let file_path = builder.add_text_field(FILE_PATH_FIELD, STRING | STORED);
        let modified_ts = builder.add_u64_field(MODIFIED_TS_FIELD, numeric_options);
        let session_json = builder.add_text_field(SESSION_JSON_FIELD, stored_text);

        Self {
            schema: builder.build(),
            content,
            user,
            agent,
            tool_call,
            tool_result,
            dirs,
            files,
            paths,
            vis_always,
            vis_user,
            vis_agent,
            vis_tool_call,
            vis_tool_result,
            vis_tool_call_result,
            vis_project_docs,
            vis_user_project_docs,
            vis_user_skill,
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
            user: schema.get_field(USER_FIELD).context("missing user field")?,
            agent: schema
                .get_field(AGENT_FIELD)
                .context("missing agent field")?,
            tool_call: schema
                .get_field(TOOL_CALL_FIELD)
                .context("missing toolcall field")?,
            tool_result: schema
                .get_field(TOOL_RESULT_FIELD)
                .context("missing toolresult field")?,
            dirs: schema.get_field(DIRS_FIELD).context("missing dirs field")?,
            files: schema
                .get_field(FILES_FIELD)
                .context("missing files field")?,
            paths: schema
                .get_field(PATHS_FIELD)
                .context("missing paths field")?,
            vis_always: schema
                .get_field(VIS_ALWAYS_FIELD)
                .context("missing visibility field")?,
            vis_user: schema
                .get_field(VIS_USER_FIELD)
                .context("missing visibility field")?,
            vis_agent: schema
                .get_field(VIS_AGENT_FIELD)
                .context("missing visibility field")?,
            vis_tool_call: schema
                .get_field(VIS_TOOL_CALL_FIELD)
                .context("missing visibility field")?,
            vis_tool_result: schema
                .get_field(VIS_TOOL_RESULT_FIELD)
                .context("missing visibility field")?,
            vis_tool_call_result: schema
                .get_field(VIS_TOOL_CALL_RESULT_FIELD)
                .context("missing visibility field")?,
            vis_project_docs: schema
                .get_field(VIS_PROJECT_DOCS_FIELD)
                .context("missing visibility field")?,
            vis_user_project_docs: schema
                .get_field(VIS_USER_PROJECT_DOCS_FIELD)
                .context("missing visibility field")?,
            vis_user_skill: schema
                .get_field(VIS_USER_SKILL_FIELD)
                .context("missing visibility field")?,
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
