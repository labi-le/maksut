use crate::prelude::*;

pub(crate) fn cleanup_old_downloads(download_dir: &Path) {
    let entries = match fs::read_dir(download_dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(
                "Unable to scan {} for startup cleanup: {error}",
                download_dir.display()
            );
            return;
        }
    };
    let now = SystemTime::now();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::error!(
                    "Unable to read an entry in {} for startup cleanup: {error}",
                    download_dir.display()
                );
                continue;
            }
        };
        if !entry.file_name().as_encoded_bytes().starts_with(b"post-") {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::error!(
                    "Unable to inspect {} for startup cleanup: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        if !metadata.is_dir() {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                tracing::error!(
                    "Unable to read modification time of {} for startup cleanup: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        if now
            .duration_since(modified)
            .is_ok_and(|age| age > Duration::from_secs(config().cleanup.max_age_secs))
        {
            if let Err(error) = fs::remove_dir_all(entry.path()) {
                tracing::error!(
                    "Unable to remove {} during startup cleanup: {error}",
                    entry.path().display()
                );
            }
        }
    }
}
