use std::collections::BTreeSet;
use std::fs;

use aics::logging::{self, LoggingMode};
use anyhow::Result;

// A separate integration-test process owns the global subscriber and log bridge.
#[test]
fn dependency_logs_keep_their_target_and_are_filtered_without_duplicates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::env::set_var("AICS_CONFIG_ROOT", temp.path());
    std::env::set_var("RUST_LOG", "aics=off,synthetic_dependency=warn");
    let first_date = chrono::Utc::now().date_naive();
    let logging = logging::init(LoggingMode::Interactive)?;
    let paths = logging.paths.clone().expect("managed log paths");

    std::thread::spawn(|| {
        log::warn!(target: "synthetic_dependency::worker", "legacy-warning");
        log::info!(target: "synthetic_dependency::worker", "legacy-info");
        log::warn!(target: "aics", "disabled-legacy-warning");
        tracing::warn!(target: "aics", "disabled-native-warning");
    })
    .join()
    .unwrap();
    drop(logging);

    let mut output = String::new();
    for date in BTreeSet::from([first_date, chrono::Utc::now().date_naive()]) {
        let file = paths.main_for_date(date);
        if file.exists() {
            output.push_str(&fs::read_to_string(file)?);
        }
    }
    assert_eq!(output.matches("legacy-warning").count(), 1, "{output}");
    assert!(output.contains("synthetic_dependency::worker"), "{output}");
    assert!(!output.contains("legacy-info"), "{output}");
    assert!(!output.contains("disabled-"), "{output}");
    assert!(!output.contains('\x1b'), "{output}");
    Ok(())
}
