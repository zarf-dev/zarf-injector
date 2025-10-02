// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2021-Present The Zarf Authors

use std::env;
use std::fs;
use std::path::PathBuf;

use axum::{
    Router,
    body::Body,
    extract::{Path, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use hex::ToHex;
use regex_lite::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::io::ReaderStream;
const OCI_MIME_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const ZARF_SEED_DIR: &str = "/zarf-seed";

/// Starts a docker compliant registry server that serves images from the seed directory
///
/// (which is a OCI image layout):
///
/// index.json - the image index
/// blobs/sha256/<sha256sum> - the image layers
/// oci-layout - the OCI image layout
fn start_seed_registry() -> Router {
    // The name and reference parameter identify the image
    // The reference may include a tag or digest.
    Router::new()
        .route(
            "/v2/*path",
            get(handler)
                .put(put_handler)
                .head(head_handler)
                .post(post_handler)
                .patch(patch_handler),
        )
        .route(
            "/v2/",
            get(|| async {
                Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json; charset=utf-8")
                    .header("Docker-Distribution-Api-Version", "registry/2.0")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .route(
            "/v2",
            get(|| async {
                Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json; charset=utf-8")
                    .header("Docker-Distribution-Api-Version", "registry/2.0")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Body::empty())
                    .unwrap()
            }),
        )
}

async fn handler(Path(path): Path<String>) -> Response {
    println!("request: {}", path);
    let path = &path;
    let manifest = Regex::new("(.+)/manifests/(.+)").unwrap();
    let blob = Regex::new(".+/([^/]+)").unwrap();

    if manifest.is_match(path) {
        let caps = manifest.captures(path).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        let reference = caps.get(2).unwrap().as_str().to_string();
        handle_get_manifest(name, reference).await
    } else if blob.is_match(path) {
        let caps = blob.captures(path).unwrap();
        let tag = caps.get(1).unwrap().as_str().to_string();
        handle_get_digest(tag).await
    } else {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body("Not Found".to_string())
            .unwrap()
            .into_response()
    }
}

/// Handles the GET request for the manifest (only returns a OCI manifest regardless of Accept header)
async fn handle_get_manifest(name: String, reference: String) -> Response {
    let root = PathBuf::from(
        std::env::var("ZARF_INJECTOR_SEED_ROOT").unwrap_or_else(|_| String::from(ZARF_SEED_DIR)),
    );

    let index = fs::read_to_string(root.join("index.json")).expect("index.json is read");
    let json: Value = serde_json::from_str(&index).expect("unable to parse index.json");

    let mut sha_manifest: String = "".to_owned();

    if reference.starts_with("sha256:") {
        sha_manifest = reference.strip_prefix("sha256:").unwrap().to_owned();
    } else {
        for manifest in json["manifests"].as_array().unwrap() {
            let image_base_name = manifest["annotations"]["org.opencontainers.image.base.name"]
                .as_str()
                .unwrap();
            let requested_reference = name.to_owned() + ":" + &reference;
            if requested_reference == image_base_name {
                sha_manifest = manifest["digest"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("sha256:")
                    .unwrap()
                    .to_owned();
            }
        }
    }
    if sha_manifest.is_empty() {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body("Not Found".to_string())
            .unwrap()
            .into_response()
    } else {
        let file_path = root.join("blobs").join("sha256").join(&sha_manifest);
        let media_type_manifest = match fs::read_to_string(&file_path) {
            Ok(content) => match serde_json::from_str::<Value>(&content) {
                Ok(file_json) => file_json["mediaType"]
                    .as_str()
                    .unwrap_or(OCI_MIME_TYPE)
                    .to_owned(),
                Err(_) => {
                    return Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body("Invalid manifest format".to_string())
                        .unwrap()
                        .into_response();
                }
            },
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body("Not Found".to_string())
                    .unwrap()
                    .into_response();
            }
        };
        match tokio::fs::File::open(&file_path).await {
            Ok(file) => {
                let metadata = match file.metadata().await {
                    Ok(meta) => meta,
                    Err(_) => {
                        return Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body("Failed to get file metadata".into())
                            .unwrap();
                    }
                };
                let stream = ReaderStream::new(file);
                Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", media_type_manifest)
                    .header("Content-Length", metadata.len())
                    .header(
                        "Docker-Content-Digest",
                        format!("sha256:{}", sha_manifest.clone()),
                    )
                    .header("Etag", format!("sha256:{}", sha_manifest))
                    .header("Docker-Distribution-Api-Version", "registry/2.0")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
            Err(err) => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(format!("File not found: {}", err))
                .unwrap()
                .into_response(),
        }
    }
}

/// Handles the GET request for a blob
async fn handle_get_digest(tag: String) -> Response {
    let root = PathBuf::from(
        std::env::var("ZARF_INJECTOR_SEED_ROOT").unwrap_or_else(|_| String::from(ZARF_SEED_DIR)),
    );
    let blob_root = root.join("blobs").join("sha256");
    let path = blob_root.join(tag.strip_prefix("sha256:").unwrap());

    match tokio::fs::File::open(&path).await {
        Ok(file) => {
            let stream = ReaderStream::new(file);
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/octet-stream")
                .header("Docker-Content-Digest", tag.to_owned())
                .header("Etag", tag.to_owned())
                .header("Docker-Distribution-Api-Version", "registry/2.0")
                .header("Cache-Control", "max-age=31536000")
                .body(Body::from_stream(stream))
                .unwrap()
        }
        Err(err) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(format!("File not found: {}", err))
            .unwrap()
            .into_response(),
    }
}

async fn put_handler(Path(path): Path<String>, request: Request) -> Response {
    let query_string = request.uri().query().unwrap_or("");
    println!("PUT request: {} query: {}", path, query_string);
    let manifest_re = Regex::new("(.+)/manifests/(.+)").unwrap();
    let blob_re = Regex::new("(.+)/blobs/uploads/(.+)").unwrap();

    if manifest_re.is_match(&path) {
        let caps = manifest_re.captures(&path).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        let reference = caps.get(2).unwrap().as_str().to_string();
        handle_put_manifest(name, reference, request).await
    } else if blob_re.is_match(&path) {
        let caps = blob_re.captures(&path).unwrap();
        let upload_id = caps.get(2).unwrap().as_str().to_string();
        handle_put_blob(upload_id, query_string.to_string(), request).await
    } else {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body("Not Found".to_string())
            .unwrap()
            .into_response()
    }
}

async fn post_handler(Path(path): Path<String>) -> Response {
    println!("POST request: {}", path);
    let blob_upload_re = Regex::new("(.+)/blobs/uploads/?$").unwrap();

    if blob_upload_re.is_match(&path) {
        handle_post_blob_upload(path).await
    } else {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body("Not Found".to_string())
            .unwrap()
            .into_response()
    }
}

async fn head_handler(Path(path): Path<String>) -> Response {
    println!("HEAD request: {}", path);
    let manifest_re = Regex::new("(.+)/manifests/(.+)").unwrap();
    let blob_re = Regex::new(".+/blobs/(.+)").unwrap();

    if manifest_re.is_match(&path) {
        let caps = manifest_re.captures(&path).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        let reference = caps.get(2).unwrap().as_str().to_string();
        handle_head_manifest(name, reference).await
    } else if blob_re.is_match(&path) {
        let caps = blob_re.captures(&path).unwrap();
        let digest = caps.get(1).unwrap().as_str().to_string();
        handle_head_blob(digest).await
    } else {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    }
}

async fn patch_handler(Path(path): Path<String>, request: Request) -> Response {
    println!("PATCH request: {}", path);
    let blob_re = Regex::new("(.+)/blobs/uploads/(.+)").unwrap();

    if blob_re.is_match(&path) {
        let caps = blob_re.captures(&path).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        let upload_id = caps.get(2).unwrap().as_str().to_string();
        handle_patch_blob(name, upload_id, request).await
    } else {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body("Not Found".to_string())
            .unwrap()
            .into_response()
    }
}

async fn handle_put_manifest(name: String, reference: String, request: Request) -> Response {
    let root = PathBuf::from(
        std::env::var("ZARF_INJECTOR_SEED_ROOT").unwrap_or_else(|_| String::from("/zarf-seed")),
    );

    // Read the body
    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body("Failed to read body".into())
                .unwrap();
        }
    };

    // Calculate digest
    let mut hasher = Sha256::new();
    hasher.update(&body_bytes);
    let digest = hasher.finalize();
    let digest_str = format!("sha256:{}", digest.encode_hex::<String>());

    // Write manifest to blobs
    let blob_path = root
        .join("blobs")
        .join("sha256")
        .join(digest_str.strip_prefix("sha256:").unwrap());
    if let Err(_) = tokio::fs::create_dir_all(blob_path.parent().unwrap()).await {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body("Failed to create directory".into())
            .unwrap();
    }

    if let Err(_) = tokio::fs::write(&blob_path, &body_bytes).await {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body("Failed to write manifest".into())
            .unwrap();
    }

    // Update index.json
    let index_path = root.join("index.json");
    let mut index: Value = match tokio::fs::read_to_string(&index_path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| {
            serde_json::json!({
                "schemaVersion": 2,
                "manifests": []
            })
        }),
        Err(_) => serde_json::json!({
            "schemaVersion": 2,
            "manifests": []
        }),
    };

    // Parse the manifest to get its mediaType
    let manifest_media_type =
        if let Ok(manifest_json) = serde_json::from_slice::<Value>(&body_bytes) {
            manifest_json
                .get("mediaType")
                .and_then(|v| v.as_str())
                .unwrap_or(OCI_MIME_TYPE)
                .to_string()
        } else {
            OCI_MIME_TYPE.to_string()
        };

    // Add or update manifest entry
    let image_name = format!("{}:{}", name, reference);
    let manifest_entry = serde_json::json!({
        "mediaType": manifest_media_type,
        "digest": digest_str,
        "size": body_bytes.len(),
        "annotations": {
            "org.opencontainers.image.base.name": image_name
        }
    });

    if let Some(manifests) = index["manifests"].as_array_mut() {
        // Remove existing entry with same name if it exists
        manifests.retain(|m| {
            m["annotations"]["org.opencontainers.image.base.name"].as_str() != Some(&image_name)
        });
        manifests.push(manifest_entry);
    }

    if let Err(_) =
        tokio::fs::write(&index_path, serde_json::to_string_pretty(&index).unwrap()).await
    {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body("Failed to update index".into())
            .unwrap();
    }

    Response::builder()
        .status(StatusCode::CREATED)
        .header("Docker-Content-Digest", digest_str.clone())
        .header("Location", format!("/v2/{}/manifests/{}", name, digest_str))
        .header("Docker-Distribution-Api-Version", "registry/2.0")
        .body(Body::empty())
        .unwrap()
}

async fn handle_post_blob_upload(path: String) -> Response {
    // Generate a simple unique ID for the upload session using timestamp and process id
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = std::process::id();
    let upload_id = format!("{}-{}", timestamp, pid);
    let location = format!("/v2/{}/{}", path.trim_end_matches('/'), upload_id);

    Response::builder()
        .status(StatusCode::ACCEPTED)
        .header("Location", location)
        .header("Docker-Distribution-Api-Version", "registry/2.0")
        .body(Body::empty())
        .unwrap()
}

async fn handle_put_blob(upload_id: String, query_string: String, request: Request) -> Response {
    let root = PathBuf::from(
        std::env::var("ZARF_INJECTOR_SEED_ROOT").unwrap_or_else(|_| String::from("/zarf-seed")),
    );

    // Read the body bytes from the request
    let request_body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body("Failed to read body".into())
                .unwrap();
        }
    };

    // Try to read from temporary file first (from PATCH), otherwise use request body
    let temp_path: PathBuf = root.join(".uploads").join(&upload_id);
    let body_bytes = if temp_path.exists() {
        match tokio::fs::read(&temp_path).await {
            Ok(bytes) => bytes,
            Err(_) => request_body_bytes.to_vec(),
        }
    } else {
        request_body_bytes.to_vec()
    };

    // Extract digest from query parameter (e.g., digest=sha256:abc123)
    // Note: query parameter is URL-encoded, so we need to decode it
    let digest_str = if query_string.contains("digest=") {
        let encoded = query_string
            .split('&')
            .find(|param| param.starts_with("digest="))
            .and_then(|param| param.strip_prefix("digest="))
            .unwrap_or("");
        // Simple URL decode for the colon
        encoded.replace("%3A", ":").replace("%3a", ":")
    } else {
        // Calculate digest
        let mut hasher = Sha256::new();
        hasher.update(&body_bytes);
        let digest = hasher.finalize();
        format!("sha256:{}", digest.encode_hex::<String>())
    };

    if digest_str.is_empty() {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body("Missing digest".into())
            .unwrap();
    }

    // Verify digest
    let mut hasher = Sha256::new();
    hasher.update(&body_bytes);
    let actual_digest = format!("sha256:{}", hasher.finalize().encode_hex::<String>());

    if digest_str != actual_digest {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(
                format!(
                    "Digest mismatch: expected {} got {}",
                    digest_str, actual_digest
                )
                .into(),
            )
            .unwrap();
    }

    // Write blob
    let blob_path = root
        .join("blobs")
        .join("sha256")
        .join(digest_str.strip_prefix("sha256:").unwrap());
    if let Err(_) = tokio::fs::create_dir_all(blob_path.parent().unwrap()).await {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body("Failed to create directory".into())
            .unwrap();
    }

    if let Err(_) = tokio::fs::write(&blob_path, &body_bytes).await {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body("Failed to write blob".into())
            .unwrap();
    }

    // Clean up temporary file
    if temp_path.exists() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }

    Response::builder()
        .status(StatusCode::CREATED)
        .header("Docker-Content-Digest", digest_str.clone())
        .header("Location", format!("/v2/blobs/{}", digest_str))
        .header("Docker-Distribution-Api-Version", "registry/2.0")
        .body(Body::empty())
        .unwrap()
}

async fn handle_head_manifest(name: String, reference: String) -> Response {
    let root = PathBuf::from(
        std::env::var("ZARF_INJECTOR_SEED_ROOT").unwrap_or_else(|_| String::from("/zarf-seed")),
    );

    let index = match fs::read_to_string(root.join("index.json")) {
        Ok(content) => content,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        }
    };

    let json: Value = match serde_json::from_str(&index) {
        Ok(j) => j,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap();
        }
    };

    let mut sha_manifest: String = "".to_owned();
    let mut media_type = OCI_MIME_TYPE.to_string();

    if reference.starts_with("sha256:") {
        sha_manifest = reference.strip_prefix("sha256:").unwrap().to_owned();
        // Find media type from index
        for manifest in json["manifests"].as_array().unwrap_or(&vec![]) {
            if let Some(digest) = manifest["digest"].as_str() {
                if digest == format!("sha256:{}", sha_manifest) {
                    media_type = manifest["mediaType"]
                        .as_str()
                        .unwrap_or(OCI_MIME_TYPE)
                        .to_string();
                    break;
                }
            }
        }
    } else {
        for manifest in json["manifests"].as_array().unwrap_or(&vec![]) {
            if let Some(image_base_name) =
                manifest["annotations"]["org.opencontainers.image.base.name"].as_str()
            {
                let requested_reference = format!("{}:{}", name, reference);
                if requested_reference == image_base_name {
                    if let Some(digest) = manifest["digest"].as_str() {
                        sha_manifest = digest.strip_prefix("sha256:").unwrap_or(digest).to_owned();
                    }
                    media_type = manifest["mediaType"]
                        .as_str()
                        .unwrap_or(OCI_MIME_TYPE)
                        .to_string();
                    break;
                }
            }
        }
    }

    if sha_manifest.is_empty() {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()
    } else {
        let file_path = root.join("blobs").join("sha256").join(&sha_manifest);
        match fs::metadata(&file_path) {
            Ok(metadata) => Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", media_type)
                .header("Content-Length", metadata.len())
                .header("Docker-Content-Digest", format!("sha256:{}", sha_manifest))
                .header("Docker-Distribution-Api-Version", "registry/2.0")
                .body(Body::empty())
                .unwrap(),
            Err(_) => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap(),
        }
    }
}

async fn handle_head_blob(digest: String) -> Response {
    let root = PathBuf::from(
        std::env::var("ZARF_INJECTOR_SEED_ROOT").unwrap_or_else(|_| String::from("/zarf-seed")),
    );
    let blob_path = root
        .join("blobs")
        .join("sha256")
        .join(digest.strip_prefix("sha256:").unwrap());

    match fs::metadata(&blob_path) {
        Ok(metadata) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", metadata.len())
            .header("Docker-Content-Digest", digest.clone())
            .header("Docker-Distribution-Api-Version", "registry/2.0")
            .body(Body::empty())
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap(),
    }
}

async fn handle_patch_blob(name: String, upload_id: String, request: Request) -> Response {
    let root = PathBuf::from(
        std::env::var("ZARF_INJECTOR_SEED_ROOT").unwrap_or_else(|_| String::from("/zarf-seed")),
    );

    // Get Content-Range header to validate upload order
    let content_range = request
        .headers()
        .get("Content-Range")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Read the body
    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body("Failed to read body".into())
                .unwrap();
        }
    };

    // Store the upload in a temporary location
    let temp_dir = root.join(".uploads");
    if let Err(_) = tokio::fs::create_dir_all(&temp_dir).await {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body("Failed to create temp directory".into())
            .unwrap();
    }

    let temp_path = temp_dir.join(&upload_id);

    // Get the current size of existing data
    let existing_size = if temp_path.exists() {
        match tokio::fs::metadata(&temp_path).await {
            Ok(meta) => meta.len() as usize,
            Err(_) => 0,
        }
    } else {
        0
    };

    // Validate Content-Range if provided
    if let Some(range) = content_range {
        // Parse Content-Range header (Example: "0-1000")
        let range_re = Regex::new(r"^(\d+)-(\d+)$").unwrap();
        if let Some(caps) = range_re.captures(&range) {
            let start: usize = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            let end: usize = caps.get(2).unwrap().as_str().parse().unwrap_or(0);

            // Validate that start matches existing_size
            if start != existing_size {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header("Range", format!("0-{}", existing_size.saturating_sub(1)))
                    .body("Chunk out of order".into())
                    .unwrap();
            }

            // Validate that the chunk size matches end - start + 1
            let expected_size = end - start + 1;
            if body_bytes.len() != expected_size {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body("Content-Length does not match Content-Range".into())
                    .unwrap();
            }
        }
    }

    // Append data to the temporary file
    if existing_size > 0 {
        // Read existing data, append new data, and write back
        match tokio::fs::read(&temp_path).await {
            Ok(mut existing_data) => {
                existing_data.extend_from_slice(&body_bytes);
                if let Err(_) = tokio::fs::write(&temp_path, &existing_data).await {
                    return Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body("Failed to write temp file".into())
                        .unwrap();
                }
            }
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body("Failed to read existing temp file".into())
                    .unwrap();
            }
        }
    } else {
        // No existing data, just write the new data
        if let Err(_) = tokio::fs::write(&temp_path, &body_bytes).await {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body("Failed to write temp file".into())
                .unwrap();
        }
    }

    // Calculate new total size (end-of-range is the position of the last byte)
    let new_total_size = existing_size + body_bytes.len();
    let end_of_range = new_total_size.saturating_sub(1);

    let location = format!("/v2/{}/blobs/uploads/{}", name, upload_id);

    Response::builder()
        .status(StatusCode::ACCEPTED)
        .header("Location", location)
        .header("Range", format!("0-{}", end_of_range))
        .header("Docker-Distribution-Api-Version", "registry/2.0")
        .body(Body::empty())
        .unwrap()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = env::args().collect();

    let bind_addr = args.get(1).map(|s| s.as_str()).unwrap_or("0.0.0.0:5000");

    let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, start_seed_registry()).await.unwrap();
}

#[cfg(test)]
mod test {
    use anyhow::{Context as _, Ok, Result, bail};
    use bollard::{Docker, image::CreateImageOptions};
    use flate2::{Compression, write::GzEncoder};
    use futures_util::{TryStreamExt, future::ready};
    use regex_lite::Regex;
    use serial_test::serial;
    use std::{
        fs::File,
        io::{Cursor, Seek, Write},
        path::{Path, PathBuf},
    };

    use crate::{OCI_MIME_TYPE, start_seed_registry};

    struct EnvGuard {
        key: String,
    }

    impl EnvGuard {
        fn new(key: &str, value: &str) -> Self {
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_string(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var(&self.key);
            }
        }
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Result<Self> {
            let path = std::env::temp_dir().join(format!("zarf-test-{}", std::process::id()));
            std::fs::create_dir_all(&path).context("should have created temporary directory")?;
            Ok(Self { path })
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    const TEST_IMAGE: &str = "ghcr.io/zarf-dev/doom-game:0.0.1";
    const DOCKER_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";
    // Based on upstream rust-oci-client regex:
    // https://github.com/oras-project/rust-oci-client/blob/657c1caf9e99ce2184a96aa319fde4f4a8c09439/src/regexp.rs#L3-L5
    const REFERENCE_REGEXP: &str = r"^((?:(?:[a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9-]*[a-zA-Z0-9])(?:(?:\.(?:[a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9-]*[a-zA-Z0-9]))+)?(?::[0-9]+)?/)?[a-z0-9]+(?:(?:(?:[._]|__|[-]*)[a-z0-9]+)+)?(?:(?:/[a-z0-9]+(?:(?:(?:[._]|__|[-]*)[a-z0-9]+)+)?)+)?)(?::([\w][\w.-]{0,127}))?(?:@([A-Za-z][A-Za-z0-9]*(?:[-_+.][A-Za-z][A-Za-z0-9]*)*[:][[:xdigit:]]{32,}))?$";

    #[tokio::test]
    #[serial]
    async fn test_pull_mt() {
        let media_types = [OCI_MIME_TYPE, DOCKER_MEDIA_TYPE];
        for media_type in media_types {
            test_pull(TEST_IMAGE, media_type).await;
        }
    }

    async fn test_pull(image: &str, media_type: &str) {
        let registry = TestRegistry::new(image).await;

        // Assert the files and directory we expect to exist do exist
        assert!(Path::new(&registry.output_root.join("index.json")).exists());
        assert!(Path::new(&registry.output_root.join("manifest.json")).exists());
        assert!(Path::new(&registry.output_root.join("oci-layout")).exists());
        assert!(Path::new(&registry.output_root.join("repositories")).exists());

        change_manifest_media_type(&registry.output_root, media_type)
            .expect("should have changed the mediaType of the manifest");

        let docker = Docker::connect_with_socket_defaults()
            .expect("should have been able to create a Docker client");

        let image_name = extract_name(image);
        let test_image = format!("127.0.0.1:{}/{}", registry.random_port, image_name);

        let test_image_pull = docker
            .create_image(
                Some(CreateImageOptions {
                    from_image: test_image.clone(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await;
        assert!(test_image_pull.is_ok());
        docker
            .remove_image(&test_image, None, None)
            .await
            .expect("should have cleaned up the pulled test image");
    }

    struct TestRegistry {
        random_port: u16,
        output_root: PathBuf,
        _seed_guard: EnvGuard,
        _tmpdir: TempDir,
    }

    impl TestRegistry {
        async fn new(image: &str) -> Self {
            let tmpdir = TempDir::new().expect("should have created temporary directory");

            let docker = Docker::connect_with_socket_defaults()
                .expect("should have been able to create a Docker client");

            let env = TestEnv::new(docker.clone(), image, tmpdir.path())
                .await
                .expect("should have setup the test environment");

            let output_root = env.seed_dir();
            let _seed_guard =
                EnvGuard::new("ZARF_INJECTOR_SEED_ROOT", &output_root.to_string_lossy());

            localize_test_image(image, &output_root)
                .expect("should have localized the test image's index.json");

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("should have been able to bind listener to a random port on localhost");
            let random_port = listener
                .local_addr()
                .expect("should have been able to resolve the address")
                .port();

            tokio::spawn(async {
                let app = start_seed_registry();
                axum::serve(listener, app)
                    .await
                    .expect("should have been able to start serving the registry");
            });

            for _ in 0..10 {
                if tokio::net::TcpStream::connect(format!("127.0.0.1:{}", random_port))
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }

            Self {
                random_port,
                output_root,
                _seed_guard,
                _tmpdir: tmpdir,
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_push() {
        test_push_with_tag().await;
        test_push_with_sha().await;
    }

    async fn test_push_with_tag() {
        let registry = TestRegistry::new(TEST_IMAGE).await;
        let docker = Docker::connect_with_socket_defaults()
            .expect("should have been able to create a Docker client");

        let test_image =
            TEST_IMAGE.replace("ghcr.io", &format!("127.0.0.1:{}", registry.random_port));
        docker
            .create_image(
                Some(CreateImageOptions {
                    from_image: test_image.clone(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .expect("should have pulled test image");

        let pushed_image = format!(
            "127.0.0.1:{}/zarf-dev/doom-game:pushed-test",
            registry.random_port
        );
        docker
            .tag_image(
                &test_image,
                Some(bollard::image::TagImageOptions {
                    repo: format!("127.0.0.1:{}/zarf-dev/doom-game", registry.random_port),
                    tag: "pushed-test".to_string(),
                }),
            )
            .await
            .expect("should have tagged image");

        use bollard::image::PushImageOptions;
        let push_result = docker
            .push_image(
                &pushed_image,
                Some(PushImageOptions {
                    tag: "pushed-test".to_string(),
                    ..Default::default()
                }),
                None,
            )
            .try_collect::<Vec<_>>()
            .await;
        assert!(
            push_result.is_ok(),
            "should have pushed image to registry: {:?}",
            push_result
        );

        docker
            .remove_image(&pushed_image, None, None)
            .await
            .expect("should have removed local copy");

        let verify_pull = docker
            .create_image(
                Some(CreateImageOptions {
                    from_image: pushed_image.clone(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await;
        assert!(
            verify_pull.is_ok(),
            "should have pulled pushed image back: {:?}",
            verify_pull
        );

        docker
            .remove_image(&test_image, None, None)
            .await
            .expect("should have cleaned up test image");
        docker
            .remove_image(&pushed_image, None, None)
            .await
            .expect("should have cleaned up pushed image");
    }

    async fn test_push_with_sha() {
        let registry = TestRegistry::new(TEST_IMAGE).await;
        let docker = Docker::connect_with_socket_defaults()
            .expect("should have been able to create a Docker client");

        let test_image =
            TEST_IMAGE.replace("ghcr.io", &format!("127.0.0.1:{}", registry.random_port));
        docker
            .create_image(
                Some(CreateImageOptions {
                    from_image: test_image.clone(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .expect("should have pulled test image");

        let index_content = tokio::fs::read_to_string(registry.output_root.join("index.json"))
            .await
            .expect("should read index.json");
        let index_json: serde_json::Value =
            serde_json::from_str(&index_content).expect("should parse index.json");
        let manifest_digest = index_json["manifests"][0]["digest"]
            .as_str()
            .expect("should have digest")
            .to_string();

        let pushed_image_by_digest = format!(
            "127.0.0.1:{}/zarf-dev/doom-game@{}",
            registry.random_port, manifest_digest
        );

        let verify_pull = docker
            .create_image(
                Some(CreateImageOptions {
                    from_image: pushed_image_by_digest.clone(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await;
        assert!(
            verify_pull.is_ok(),
            "should have pulled image with SHA: {:?}",
            verify_pull
        );

        docker
            .remove_image(&test_image, None, None)
            .await
            .expect("should have cleaned up test image");
        let _ = docker
            .remove_image(&pushed_image_by_digest, None, None)
            .await;
    }

    #[tokio::test]
    #[serial]
    async fn test_multi_chunk_upload() {
        use sha2::{Digest, Sha256};

        let registry = TestRegistry::new(TEST_IMAGE).await;
        let client = reqwest::Client::new();
        let base_url = format!("http://127.0.0.1:{}", registry.random_port);

        // Create test data (1MB)
        let chunk1 = vec![1u8; 512 * 1024];
        let chunk2 = vec![2u8; 512 * 1024];
        let all_data = [chunk1.clone(), chunk2.clone()].concat();

        // Calculate digest
        let mut hasher = Sha256::new();
        hasher.update(&all_data);
        let digest = format!("sha256:{}", hex::encode(hasher.finalize()));

        // POST to start upload
        let resp = client
            .post(&format!("{}/v2/test/blobs/uploads/", base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 202);
        let location = resp.headers().get("Location").unwrap().to_str().unwrap();

        // PATCH chunk 1
        let resp = client
            .patch(&format!("{}{}", base_url, location))
            .header("Content-Range", "0-524287")
            .header("Content-Length", chunk1.len())
            .body(chunk1)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 202);
        assert_eq!(resp.headers().get("Range").unwrap(), "0-524287");
        let location = resp.headers().get("Location").unwrap().to_str().unwrap();

        // PATCH chunk 2
        let resp = client
            .patch(&format!("{}{}", base_url, location))
            .header("Content-Range", "524288-1048575")
            .header("Content-Length", chunk2.len())
            .body(chunk2)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 202);
        assert_eq!(resp.headers().get("Range").unwrap(), "0-1048575");
        let location = resp.headers().get("Location").unwrap().to_str().unwrap();

        // PUT to close
        let resp = client
            .put(&format!("{}{}?digest={}", base_url, location, digest))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }

    // This localizes the test image's index.json such that the registry server
    // will be able to match the test image from it
    fn localize_test_image(image_reference: &str, image_root: &Path) -> Result<()> {
        let reference: String = normalize_manifest_reference(image_reference)
            .context("should have localized the test image reference")?;

        let mut index_file = File::options()
            .read(true)
            .write(true)
            .open(image_root.join("index.json"))
            .context("should have opened index.json")?;

        let mut index_json: serde_json::Value =
            serde_json::from_reader(index_file.try_clone().unwrap())
                .context("should have read index.json")?;

        // Overwrite or add an annotation for "org.opencontainers.image.base.name"
        // that is normalized to be without registry address so that it can be
        // pulled locally
        index_json
            .get_mut("manifests")
            .and_then(|manifests| manifests.get_mut(0))
            .and_then(|array| array.get_mut("annotations"))
            .and_then(|annotations| annotations.as_object_mut())
            .and_then(|annotations| {
                annotations.insert(
                    "org.opencontainers.image.base.name".into(),
                    reference.into(),
                )
            });

        // Rewind index.json so serde overwrites from beginning of the file instead of appending to the end
        index_file.rewind().unwrap();
        serde_json::to_writer(index_file.try_clone().unwrap(), &index_json)
            .context("should have overwrote index.json")?;
        Ok(())
    }

    // Changes the mediaType in the manifest file
    fn change_manifest_media_type(output_root: &Path, new_media_type: &str) -> Result<()> {
        // Read the index.json to get the manifest digest
        let index_file =
            File::open(output_root.join("index.json")).context("should have opened index.json")?;

        let index_json: serde_json::Value =
            serde_json::from_reader(index_file).context("should have read index.json")?;

        // Get the digest from manifests[0]
        let sha_manifest = index_json["manifests"][0]["digest"]
            .as_str()
            .context("should have found digest in manifest")?
            .strip_prefix("sha256:")
            .context("should have stripped sha256: prefix")?;

        // Open the manifest file
        let manifest_path = output_root.join("blobs").join("sha256").join(sha_manifest);
        let mut manifest_file = File::options()
            .read(true)
            .write(true)
            .open(&manifest_path)
            .context("should have opened manifest file")?;

        // Read and parse the manifest
        let mut manifest_json: serde_json::Value =
            serde_json::from_reader(manifest_file.try_clone().unwrap())
                .context("should have read manifest.json")?;

        // Change the mediaType
        manifest_json["mediaType"] = new_media_type.into();

        // Rewind and write back
        manifest_file.rewind().unwrap();
        serde_json::to_writer(manifest_file, &manifest_json)
            .context("should have written updated manifest")?;

        Ok(())
    }

    // "Normalizes" the image reference by removing the registry component from it,
    // so that it can be used for referring to local images.
    fn normalize_manifest_reference(identifier: &str) -> Result<String> {
        let re = Regex::new(REFERENCE_REGEXP)?;
        let caps = re
            .captures(identifier)
            .context("should have matched captures for extracting reference components")?;
        let repository = &caps[1];
        let tag = caps.get(2).map(|m| m.as_str().to_owned());
        let digest = caps.get(3).map(|m| m.as_str().to_owned());
        let reference = match (tag, digest) {
            (None, None) => "latest".into(),
            (None, Some(dgst)) => dgst,
            (Some(tg), None) => tg,
            // This should never happen, but for the sake of satisfying the borrow checker we need it here.
            _ => {
                bail!("both tag and digest were matched by the regex, that should not be possible")
            }
        };
        let name = extract_name(repository);
        Ok(format!("{name}:{reference}"))
    }

    // Based on rust-oci-client's split_domain:
    // https://github.com/oras-project/rust-oci-client/blob/657c1caf9e99ce2184a96aa319fde4f4a8c09439/src/reference.rs#L297-L330
    fn extract_name(name: &str) -> String {
        let mut domain: String;
        let mut remainder: String;

        match name.split_once('/') {
            None => {
                domain = "docker.io".into();
                remainder = name.into();
            }
            Some((left, right)) => {
                if !(left.contains('.') || left.contains(':')) && left != "localhost" {
                    domain = "docker.io".into();
                    remainder = name.into();
                } else {
                    domain = left.into();
                    remainder = right.into();
                }
            }
        }
        if domain == "index.docker.io" {
            domain = "docker.io".into();
        }
        if domain == "docker.io" && !remainder.contains('/') {
            remainder = format!("{}/{}", "library", remainder);
        }

        remainder
    }

    struct TestEnv {
        seed_dir: PathBuf,
    }

    impl TestEnv {
        async fn new(client: Docker, image: &str, root: &Path) -> Result<Self> {
            // Ensure we have test directory set up
            let seed_dir = root.join("zarf-seed");
            std::fs::create_dir(&seed_dir).context("should have created test seed directory")?;

            // Download test image
            Self::ensure_image_exists_locally(&client, image)
                .await
                .context("should have pulled down the test image")?;

            // Export test image from docker as a tarball
            let image_stream = client.export_image(image).map_err(anyhow::Error::msg);

            // Collect the tarball into memory
            let buffer = Cursor::new(Vec::new());
            let mut gz = GzEncoder::new(buffer, Compression::default());
            image_stream
                .try_for_each(|data| {
                    let res = gz.write_all(&data).map_err(anyhow::Error::msg);
                    ready(res)
                })
                .await?;

            let buffer = gz.finish().context("should have finished encoding image")?;

            // Extract the tarball directly to the seed directory
            let tar = flate2::read::GzDecoder::new(&buffer.get_ref()[..]);
            let mut archive = tar::Archive::new(tar);
            archive
                .unpack(&seed_dir)
                .context("should have unpacked image to seed directory")?;

            Ok(Self { seed_dir })
        }

        fn seed_dir(&self) -> PathBuf {
            self.seed_dir.to_owned()
        }

        async fn ensure_image_exists_locally(client: &Docker, image: &str) -> Result<()> {
            // Check if the test image already exists.
            if (client.inspect_image(image).await).is_ok() {
                Ok(())
            } else {
                let options = Some(CreateImageOptions {
                    from_image: image,
                    ..Default::default()
                });
                // Attempt to pull image from the upstream registry
                let _ = client
                    .create_image(options, None, None)
                    .try_collect::<Vec<_>>()
                    .await
                    .map_err(anyhow::Error::msg)
                    .context("should have been able to pull test image")?;
                // Inspect the image to make sure it exists locally and then discard the output
                Ok(client.inspect_image(image).await.map(|_| ())?)
            }
        }
    }
}
