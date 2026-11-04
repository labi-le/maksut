use crate::prelude::*;

pub(crate) fn cleanup_old_downloads(download_dir: &Path) {
    let entries = match fs::read_dir(download_dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(
                dir = %download_dir.display(),
                error = %error,
                "unable to scan directory for startup cleanup"
            );
            return;
        }
    };
    let now = SystemTime::now();
    let mut removed = 0usize;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::error!(
                    dir = %download_dir.display(),
                    error = %error,
                    "unable to read directory entry for startup cleanup"
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
                    path = %entry.path().display(),
                    error = %error,
                    "unable to inspect entry for startup cleanup"
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
                    path = %entry.path().display(),
                    error = %error,
                    "unable to read modification time for startup cleanup"
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
                    path = %entry.path().display(),
                    error = %error,
                    "unable to remove directory during startup cleanup"
                );
            } else {
                removed += 1;
            }
        }
    }
    tracing::info!(removed, "startup cleanup finished");
}
