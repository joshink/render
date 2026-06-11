use std::fs::{self, File};
use std::path::{Path, PathBuf};
use log::{info, warn};

/// Upper bound on the total size of the `target/cache` directory, enforced
/// after each download by evicting least-recently-used entries. Presigned
/// URLs make cache keys effectively single-use (the rotating query string is
/// part of the hashed key), so without eviction the cache grows by one full
/// asset per job until the disk fills. Override with the
/// `RENDER_CACHE_MAX_BYTES` environment variable.
const DEFAULT_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10 GiB

fn cache_max_bytes() -> u64 {
    std::env::var("RENDER_CACHE_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CACHE_MAX_BYTES)
}

/// Fetches a remote URL using ureq and caches it locally under target/cache.
/// Returns the path to the local cached file.
pub fn fetch_remote_url(url: &str) -> Result<String, String> {
    let cache_dir = Path::new("target/cache");
    fs::create_dir_all(cache_dir)
        .map_err(|e| format!("Failed to create cache directory: {}", e))?;

    let hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        hasher.finish()
    };

    // Strip query parameters to find clean extension
    let clean_url_path = url.split('?').next().unwrap_or(url);
    let extension = Path::new(clean_url_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("bin");

    let cache_path = cache_dir.join(format!("{}.{}", hash, extension));

    if cache_path.exists() {
        info!("Cache hit for remote URL: {} -> {:?}", url, cache_path);
        // Refresh the mtime (best-effort) so eviction approximates LRU
        // rather than download order.
        if let Ok(file) = File::options().append(true).open(&cache_path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        return Ok(cache_path.to_string_lossy().to_string());
    }

    info!("Cache miss, downloading remote URL: {} -> {:?}", url, cache_path);

    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTP request failed for URL {}: {}", url, e))?;

    // Download to a unique temp file and rename into place: concurrent
    // fetches of the same URL must never observe (or serve) a half-written
    // entry, and a failed transfer must not poison the cache with a
    // truncated file that every later job would treat as valid.
    let temp_path = cache_dir.join(format!(".{}-{}.part", hash, uuid::Uuid::new_v4()));
    let mut reader = response.into_reader();
    let write_result = File::create(&temp_path)
        .map_err(|e| format!("Failed to create cache file {:?}: {}", temp_path, e))
        .and_then(|mut file| {
            std::io::copy(&mut reader, &mut file)
                .map_err(|e| format!("Failed to write data to cache file: {}", e))
        });
    if let Err(e) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }
    if let Err(e) = fs::rename(&temp_path, &cache_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("Failed to move cache file into place: {}", e));
    }

    evict_cache_overflow(cache_dir, cache_max_bytes());

    Ok(cache_path.to_string_lossy().to_string())
}

/// Removes least-recently-used cache entries until the directory fits within
/// `max_bytes`. Eviction races with concurrent readers are benign on POSIX:
/// an unlinked file stays readable through any already-open handle.
fn evict_cache_overflow(cache_dir: &Path, max_bytes: u64) {
    let entries = match fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!(
                "Failed to read asset cache dir {:?} for eviction ({}); cache may grow unbounded",
                cache_dir, e
            );
            return;
        }
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            // Dot-prefixed names are in-progress downloads; leave them alone.
            let name = path.file_name()?.to_str()?;
            if name.starts_with('.') {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, meta.len(), path))
        })
        .collect();

    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= max_bytes {
        return;
    }

    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, len, path) in files {
        if total <= max_bytes {
            break;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                total = total.saturating_sub(len);
                info!("Evicted cached asset {:?} ({} bytes)", path, len);
            }
            Err(e) => warn!("Failed to evict cached asset {:?}: {}", path, e),
        }
    }
}
