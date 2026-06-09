use std::fs::File;
use std::path::Path;
use log::info;

/// Fetches a remote URL using ureq and caches it locally under target/cache.
/// Returns the path to the local cached file.
pub fn fetch_remote_url(url: &str) -> Result<String, String> {
    let cache_dir = Path::new("target/cache");
    if !cache_dir.exists() {
        std::fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {}", e))?;
    }

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
        return Ok(cache_path.to_string_lossy().to_string());
    }

    info!("Cache miss, downloading remote URL: {} -> {:?}", url, cache_path);

    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTP request failed for URL {}: {}", url, e))?;

    let mut reader = response.into_reader();
    let mut file = File::create(&cache_path)
        .map_err(|e| format!("Failed to create cache file {:?}: {}", cache_path, e))?;

    std::io::copy(&mut reader, &mut file)
        .map_err(|e| format!("Failed to write data to cache file: {}", e))?;

    Ok(cache_path.to_string_lossy().to_string())
}
