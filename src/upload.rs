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

/// Encodes bytes as standard (RFC 4648) base64. Used to build the HTTP Basic
/// auth header for the Mux API. Implemented inline to avoid pulling base64 in
/// as a direct dependency.
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Uploads a local video file to Mux Video using the Direct Uploads API.
///
/// Flow:
///   1. `POST /video/v1/uploads` (HTTP Basic auth with the Mux API token)
///      creates a direct upload and returns a one-time signed PUT URL.
///   2. The rendered file is streamed to that URL via PUT — identical in spirit
///      to [`upload_signed_url`], but Mux ingests and transcodes it into an asset.
///
/// After the PUT, fetches the upload once more to read the `asset_id` Mux
/// assigns on ingest and returns it. If the asset id is not yet assigned,
/// returns `upload:<upload_id>` as a pollable handle instead. Asset
/// *processing* (preparing → ready) is left to the caller to poll. Mux only
/// ingests video, so non-`.mp4` outputs are rejected up front.
pub fn upload_mux(
    local_path: &str,
    token_id: Option<String>,
    token_secret: Option<String>,
) -> Result<String, String> {
    if !local_path.ends_with(".mp4") {
        return Err(format!(
            "Mux only ingests video; got '{}'. Use an .mp4 output (mux:// implies video).",
            local_path
        ));
    }

    let resolved_id = token_id
        .or_else(|| std::env::var("MUX_TOKEN_ID").ok())
        .ok_or_else(|| "Mux token ID not found in spec output credentials, CLI flags, or MUX_TOKEN_ID.".to_string())?;
    let resolved_secret = token_secret
        .or_else(|| std::env::var("MUX_TOKEN_SECRET").ok())
        .ok_or_else(|| "Mux token secret not found in spec output credentials, CLI flags, or MUX_TOKEN_SECRET.".to_string())?;

    let auth = format!(
        "Basic {}",
        base64_encode(format!("{}:{}", resolved_id, resolved_secret).as_bytes())
    );

    // 1. Create a direct upload. Mux responds with a one-time signed PUT URL.
    // ureq's `json` feature is not enabled, so serialize/parse via serde_json.
    log::info!("Creating Mux direct upload...");
    let create_body = serde_json::json!({
        "cors_origin": "*",
        "new_asset_settings": { "playback_policy": ["public"] }
    })
    .to_string();

    let create_resp = match ureq::post("https://api.mux.com/video/v1/uploads")
        .set("Authorization", &auth)
        .set("Content-Type", "application/json")
        .send_string(&create_body)
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            return Err(format!("Mux create-upload failed with status code {}: {}", code, body));
        }
        Err(e) => return Err(format!("HTTP transport error creating Mux upload: {}", e)),
    };

    let resp_body = create_resp
        .into_string()
        .map_err(|e| format!("Failed to read Mux create-upload response: {}", e))?;
    let json: serde_json::Value = serde_json::from_str(&resp_body)
        .map_err(|e| format!("Failed to parse Mux create-upload response: {}", e))?;

    let put_url = json["data"]["url"]
        .as_str()
        .ok_or_else(|| format!("Mux create-upload response missing data.url: {}", json))?
        .to_string();
    let upload_id = json["data"]["id"].as_str().unwrap_or("unknown").to_string();

    // 2. Stream the rendered file to the signed PUT URL. ureq sends the File
    // handle directly so large videos never sit fully in memory.
    log::info!("Uploading video to Mux (upload id: {})...", upload_id);
    let file = File::open(local_path)
        .map_err(|e| format!("Failed to open local file {}: {}", local_path, e))?;

    match ureq::put(&put_url).set("Content-Type", "video/mp4").send(file) {
        Ok(response) => {
            log::info!("Mux upload successful. Status code: {}", response.status());
        }
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            return Err(format!("Mux upload PUT failed with status code {}: {}", code, body));
        }
        Err(e) => return Err(format!("HTTP transport error uploading to Mux: {}", e)),
    }

    // 3. Mux creates the asset asynchronously after ingest, so the upload's
    // asset_id is not populated until shortly after the PUT. Re-fetch the
    // upload a few times to read it. We deliberately do NOT wait for the
    // asset to finish processing — the caller polls the asset itself.
    match fetch_mux_asset_id(&auth, &upload_id) {
        Some(asset_id) => {
            log::info!("Mux asset created: {}", asset_id);
            Ok(asset_id)
        }
        None => {
            log::warn!(
                "Mux upload {} accepted, but asset_id not yet assigned. \
                 Poll GET /video/v1/uploads/{} to retrieve it.",
                upload_id, upload_id
            );
            // Fall back to the upload id so the caller has a handle to poll.
            Ok(format!("upload:{}", upload_id))
        }
    }
}

/// Polls `GET /video/v1/uploads/{id}` a bounded number of times to read the
/// `asset_id` Mux assigns once it begins ingesting the upload. Returns `None`
/// if it is still unassigned after the retry budget (asset creation is usually
/// near-instant, but the API is eventually-consistent).
fn fetch_mux_asset_id(auth: &str, upload_id: &str) -> Option<String> {
    let url = format!("https://api.mux.com/video/v1/uploads/{}", upload_id);
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        let resp = match ureq::get(&url).set("Authorization", auth).call() {
            Ok(resp) => resp,
            Err(e) => {
                log::warn!("Mux upload status check failed (attempt {}): {}", attempt + 1, e);
                continue;
            }
        };
        let body = match resp.into_string() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(j) => j,
            Err(_) => continue,
        };
        if let Some(asset_id) = json["data"]["asset_id"].as_str() {
            return Some(asset_id.to_string());
        }
    }
    None
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

    #[test]
    fn test_base64_encode() {
        // Standard RFC 4648 vectors, including the padding edge cases.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // Mux Basic auth shape: "token_id:token_secret".
        assert_eq!(base64_encode(b"id:secret"), "aWQ6c2VjcmV0");
    }

    #[test]
    fn test_mux_rejects_non_video() {
        let res = upload_mux("output.png", Some("id".into()), Some("secret".into()));
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Mux only ingests video"));
    }
}
