use std::fs::File;
use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::region::Region;

pub fn parse_s3_uri(uri: &str) -> Result<(String, String), String> {
    if !uri.starts_with("s3://") {
        return Err(format!("Invalid S3 URI: {}", uri));
    }
    let s3_part = &uri["s3://".len()..];
    if let Some(pos) = s3_part.find('/') {
        let bucket = s3_part[..pos].to_string();
        let key = s3_part[pos + 1..].to_string();
        if bucket.is_empty() || key.is_empty() {
            return Err(format!("Invalid S3 URI structure: {}", uri));
        }
        Ok((bucket, key))
    } else {
        Err(format!("Invalid S3 URI structure (missing key path): {}", uri))
    }
}

pub fn parse_gs_uri(uri: &str) -> Result<(String, String), String> {
    if !uri.starts_with("gs://") {
        return Err(format!("Invalid GCS URI: {}", uri));
    }
    let gs_part = &uri["gs://".len()..];
    if let Some(pos) = gs_part.find('/') {
        let bucket = gs_part[..pos].to_string();
        let key = gs_part[pos + 1..].to_string();
        if bucket.is_empty() || key.is_empty() {
            return Err(format!("Invalid GCS URI structure: {}", uri));
        }
        Ok((bucket, key))
    } else {
        Err(format!("Invalid GCS URI structure (missing key path): {}", uri))
    }
}

pub fn upload_signed_url(local_path: &str, url: &str) -> Result<(), String> {
    log::info!("Uploading to signed URL via PUT...");
    let file = File::open(local_path)
        .map_err(|e| format!("Failed to open local file {}: {}", local_path, e))?;
    
    let content_type = if local_path.ends_with(".mp4") {
        "video/mp4"
    } else if local_path.ends_with(".png") {
        "image/png"
    } else if local_path.ends_with(".jpg") || local_path.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "application/octet-stream"
    };

    // ureq 2.x returns Err(Error::Status(code, response)) for non-2xx responses,
    // so we must match on the error variant to extract the response body.
    match ureq::put(url)
        .set("Content-Type", content_type)
        .send(file)
    {
        Ok(response) => {
            log::info!("Upload successful. Status code: {}", response.status());
            Ok(())
        }
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(format!("Upload failed with status code {}: {}", code, body))
        }
        Err(e) => Err(format!("HTTP transport error: {}", e)),
    }
}

pub fn upload_s3(
    local_path: &str,
    dest: &str,
    access_key: Option<String>,
    secret_key: Option<String>,
    region_str: Option<String>,
) -> Result<(), String> {
    let (bucket_name, key) = parse_s3_uri(dest)?;
    
    // Resolve credentials
    let resolved_key = access_key.or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
        .ok_or_else(|| "AWS access key ID not found in spec output, CLI flags, or environment variables.".to_string())?;
    
    let resolved_secret = secret_key.or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok())
        .ok_or_else(|| "AWS secret access key not found in spec output, CLI flags, or environment variables.".to_string())?;
        
    let resolved_region = region_str
        .or_else(|| std::env::var("AWS_REGION").ok())
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .unwrap_or_else(|| "us-east-1".to_string());

    let region = resolved_region.parse::<Region>()
        .map_err(|e| format!("Failed to parse S3 region '{}': {}", resolved_region, e))?;

    let credentials = Credentials::new(
        Some(&resolved_key),
        Some(&resolved_secret),
        None,
        None,
        None,
    ).map_err(|e| format!("Failed to create credentials: {}", e))?;

    let bucket = Bucket::new(&bucket_name, region, credentials)
        .map_err(|e| format!("Failed to initialize bucket '{}': {}", bucket_name, e))?;

    log::info!("Uploading to S3 (bucket: {}, key: {}) via rust-s3...", bucket_name, key);

    // Stream from the file handle so large video outputs (50-200+ MB) never
    // sit in memory in full.
    let mut file = File::open(local_path)
        .map_err(|e| format!("Failed to open local file {}: {}", local_path, e))?;

    let status_code = bucket.put_object_stream(&mut file, &key)
        .map_err(|e| format!("S3 PUT failed: {}", e))?;

    if status_code < 200 || status_code >= 300 {
        return Err(format!("S3 PUT failed with status code {}", status_code));
    }

    log::info!("Upload successful.");
    Ok(())
}

pub fn upload_gcs(
    local_path: &str,
    dest: &str,
    access_key: Option<String>,
    secret_key: Option<String>,
    region_str: Option<String>,
) -> Result<(), String> {
    let (bucket_name, key) = parse_gs_uri(dest)?;
    
    // GCS supports S3-compatible HMAC keys for interoperability.
    // We fall back to AWS_ACCESS_KEY_ID as the last resort because users
    // who configure GCS S3-interop often reuse the same env var names.
    let resolved_key = access_key
        .or_else(|| std::env::var("GCS_ACCESS_KEY_ID").ok())
        .or_else(|| std::env::var("GCP_ACCESS_KEY_ID").ok())
        .or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
        .ok_or_else(|| "GCS access key ID not found in spec output, CLI flags, or environment variables.".to_string())?;
    
    let resolved_secret = secret_key
        .or_else(|| std::env::var("GCS_SECRET_ACCESS_KEY").ok())
        .or_else(|| std::env::var("GCP_SECRET_ACCESS_KEY").ok())
        .or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok()) // GCS S3-interop fallback
        .ok_or_else(|| "GCS secret access key not found in spec output, CLI flags, or environment variables.".to_string())?;
        
    let resolved_region = region_str.unwrap_or_else(|| "auto".to_string());

    let region = Region::Custom {
        region: resolved_region,
        endpoint: "https://storage.googleapis.com".to_string(),
    };

    let credentials = Credentials::new(
        Some(&resolved_key),
        Some(&resolved_secret),
        None,
        None,
        None,
    ).map_err(|e| format!("Failed to create GCS credentials: {}", e))?;

    // NOTE: rust-s3 defaults to virtual-hosted-style URLs (bucket.endpoint).
    // If the bucket name contains dots (e.g. "my.bucket.name"), TLS cert
    // validation will fail. Use `.with_path_style()` on the Bucket if needed.
    let bucket = Bucket::new(&bucket_name, region, credentials)
        .map_err(|e| format!("Failed to initialize GCS bucket '{}': {}", bucket_name, e))?;

    log::info!("Uploading to GCS (bucket: {}, key: {}) via rust-s3...", bucket_name, key);

    // Stream from the file handle so large video outputs (50-200+ MB) never
    // sit in memory in full.
    let mut file = File::open(local_path)
        .map_err(|e| format!("Failed to open local file {}: {}", local_path, e))?;

    let status_code = bucket.put_object_stream(&mut file, &key)
        .map_err(|e| format!("GCS PUT failed: {}", e))?;

    if status_code < 200 || status_code >= 300 {
        return Err(format!("GCS PUT failed with status code {}", status_code));
    }

    log::info!("Upload successful.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_s3_uri() {
        let res = parse_s3_uri("s3://my-bucket/path/to/object.mp4").unwrap();
        assert_eq!(res.0, "my-bucket");
        assert_eq!(res.1, "path/to/object.mp4");

        assert!(parse_s3_uri("s3://my-bucket").is_err());
        assert!(parse_s3_uri("invalid://my-bucket/key").is_err());
    }

    #[test]
    fn test_parse_gs_uri() {
        let res = parse_gs_uri("gs://gcs-bucket-name/folder/file.png").unwrap();
        assert_eq!(res.0, "gcs-bucket-name");
        assert_eq!(res.1, "folder/file.png");

        assert!(parse_gs_uri("gs://gcs-bucket-name").is_err());
        assert!(parse_gs_uri("s3://bucket/key").is_err());
    }
}
