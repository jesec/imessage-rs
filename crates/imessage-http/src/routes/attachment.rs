/// Attachment routes:
///   GET  /api/v1/attachment/count
///   GET  /api/v1/attachment/:guid
///   GET  /api/v1/attachment/:guid/download
///   GET  /api/v1/attachment/:guid/download/force
///   GET  /api/v1/attachment/:guid/live
///   GET  /api/v1/attachment/:guid/blurhash
///   POST /api/v1/attachment/upload
use std::path::PathBuf;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use imessage_core::config::AppPaths;
use imessage_core::utils::expand_tilde;
use imessage_private_api::actions;
use imessage_serializers::attachment::serialize_attachment;
use imessage_serializers::config::AttachmentSerializerConfig;

use crate::awaiter;
use crate::middleware::error::{AppError, success_response};
use crate::path_safety::{sanitize_filename, sanitize_header_filename};
use crate::state::AppState;

/// Live photo extensions that can have a .mov companion
const LIVE_PHOTO_EXTS: &[&str] = &["png", "jpeg", "jpg", "heic", "tiff"];

/// Get the .mov companion path for a live photo, if it exists on disk.
pub fn get_live_photo_path(file_path: &str) -> Option<PathBuf> {
    let real_path = expand_tilde(file_path);
    let file_str = real_path.to_string_lossy().to_string();

    // Handle double extension like .heic.jpeg
    let ext = if file_str.contains(".heic.jpeg") {
        "heic.jpeg".to_string()
    } else {
        real_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())?
    };

    let ext_lower = ext.split('.').next_back().unwrap_or(&ext);
    if !LIVE_PHOTO_EXTS.contains(&ext_lower) {
        return None;
    }

    let mov_path = if ext.contains('.') {
        // Double extension: replace both
        let stem = file_str.strip_suffix(&format!(".{ext}"))?;
        PathBuf::from(format!("{stem}.mov"))
    } else {
        real_path.with_extension("mov")
    };

    if mov_path.exists() {
        Some(mov_path)
    } else {
        None
    }
}

/// GET /api/v1/attachment/count
pub async fn count(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let total = state
        .imessage_repo
        .lock()
        .get_attachment_count()
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    Ok(Json(success_response(json!({ "total": total }))))
}

/// GET /api/v1/attachment/:guid
pub async fn find(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let attachment = state
        .imessage_repo
        .lock()
        .get_attachment(&guid)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::not_found("Attachment does not exist!"))?;

    let config = AttachmentSerializerConfig::default();
    let data = serialize_attachment(&attachment, &config, false);
    Ok(Json(success_response(data)))
}

/// Download query params
#[derive(Debug, Deserialize, Default)]
pub struct DownloadParams {
    pub height: Option<String>,
    pub width: Option<String>,
    pub original: Option<String>,
}

/// GET /api/v1/attachment/:guid/download
pub async fn download(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    Query(params): Query<DownloadParams>,
) -> Result<impl IntoResponse, AppError> {
    let attachment = state
        .imessage_repo
        .lock()
        .get_attachment(&guid)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::not_found("Attachment does not exist!"))?;

    let file_path = attachment
        .filename
        .as_deref()
        .ok_or_else(|| AppError::server_error("Attachment has no file path!"))?;

    let mut real_path = expand_tilde(file_path);

    if !real_path.exists() {
        return Err(AppError::server_error("Attachment does not exist on disk!"));
    }

    // Determine MIME type: try mime_type field first, then guess from transfer_name,
    // then fall back to application/octet-stream. Never use UTI as MIME (it's not valid).
    let mut mime_type = attachment
        .mime_type
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            attachment
                .transfer_name
                .as_deref()
                .and_then(|name| mime_guess::from_path(name).first().map(|m| m.to_string()))
        })
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let use_original = params
        .original
        .as_deref()
        .map(|s| matches!(s.to_lowercase().as_str(), "true" | "1"))
        .unwrap_or(false);

    if !use_original {
        // Try attachment conversion (HEIC→JPEG, CAF→M4A)
        let uti = attachment.uti.as_deref().unwrap_or("");
        let mime_ref = mime_type.as_str();

        // Use originalGuid for cache directory (originalGuid ?? guid)
        let cache_guid = attachment
            .original_guid
            .as_deref()
            .unwrap_or(&attachment.guid);

        if uti == "public.heic"
            || uti == "public.heif"
            || uti == "public.tiff"
            || mime_ref.starts_with("image/heic")
            || mime_ref.starts_with("image/heif")
            || mime_ref.starts_with("image/tiff")
        {
            let transfer_name = attachment.transfer_name.as_deref().unwrap_or("converted");
            let convert_dir = AppPaths::convert_dir().join(cache_guid);
            let output = convert_dir.join(format!("{transfer_name}.jpeg"));
            if output.exists()
                || (std::fs::create_dir_all(&convert_dir).is_ok()
                    && imessage_apple::process::convert_to_jpg(
                        file_path,
                        &output.to_string_lossy(),
                    )
                    .await
                    .is_ok())
            {
                real_path = output;
                mime_type = "image/jpeg".to_string();
            }
        } else if uti == "com.apple.coreaudio-format" || mime_ref == "audio/x-caf" {
            let transfer_name = attachment.transfer_name.as_deref().unwrap_or("audio");
            let convert_dir = AppPaths::convert_dir().join(cache_guid);
            let output = convert_dir.join(format!("{transfer_name}.m4a"));
            if output.exists()
                || (std::fs::create_dir_all(&convert_dir).is_ok()
                    && imessage_apple::process::convert_caf_to_m4a(
                        file_path,
                        &output.to_string_lossy(),
                    )
                    .await
                    .is_ok())
            {
                real_path = output;
                mime_type = "audio/mp4".to_string();
            }
        }

        // Handle resizing (images only, not GIFs)
        let parsed_width = params.width.as_deref().and_then(|s| s.parse::<u32>().ok());
        let parsed_height = params.height.as_deref().and_then(|s| s.parse::<u32>().ok());

        if mime_type.starts_with("image/")
            && mime_type != "image/gif"
            && (parsed_width.is_some() || parsed_height.is_some())
        {
            let transfer_name = attachment.transfer_name.as_deref().unwrap_or("attachment");
            let mut cache_name = transfer_name.to_string();
            if let Some(h) = parsed_height {
                cache_name += &format!(".{h}");
            }
            if let Some(w) = parsed_width {
                cache_name += &format!(".{w}");
            }

            let cache_dir = AppPaths::attachment_cache_dir().join(cache_guid);
            let cached_path = cache_dir.join(&cache_name);

            if cached_path.exists()
                || (std::fs::create_dir_all(&cache_dir).is_ok()
                    && imessage_apple::process::resize_image(
                        &real_path.to_string_lossy(),
                        &cached_path.to_string_lossy(),
                        parsed_width,
                        parsed_height,
                    )
                    .await
                    .is_ok())
            {
                real_path = cached_path;
                mime_type = "image/jpeg".to_string();
            }
        }
    }

    stream_file(&real_path, &mime_type).await
}

/// GET /api/v1/attachment/:guid/download/force [Private API]
pub async fn force_download(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    Query(params): Query<DownloadParams>,
) -> Result<impl IntoResponse, AppError> {
    let attachment = state
        .imessage_repo
        .lock()
        .get_attachment(&guid)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| {
            AppError::bad_request(&format!(
                "An attachment with the GUID, \"{guid}\" does not exist!"
            ))
        })?;

    let api = state.require_private_api()?;
    let action = actions::download_purged_attachment(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Poll until transfer_state == 5 (max 10 minutes, 5s fixed interval)
    let repo = state.imessage_repo.clone();
    let guid_clone = guid.clone();
    let downloaded = awaiter::result_awaiter(
        Duration::from_secs(5),
        1.0, // constant 5s intervals
        Duration::from_secs(600),
        || {
            let repo = repo.clone();
            let g = guid_clone.clone();
            async move { repo.lock().get_attachment(&g).ok().flatten() }
        },
        |att| att.transfer_state == 5,
    )
    .await;

    match downloaded {
        Some(att) if att.transfer_state == 5 => {
            // Delegate to normal download
            download(State(state), Path(guid), Query(params)).await
        }
        _ => Err(AppError::server_error(&format!(
            "Failed to download attachment! Transfer State: {}",
            attachment.transfer_state
        ))),
    }
}

/// GET /api/v1/attachment/:guid/live
pub async fn download_live(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let attachment = state
        .imessage_repo
        .lock()
        .get_attachment(&guid)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::not_found("Attachment does not exist!"))?;

    let file_path = attachment
        .filename
        .as_deref()
        .ok_or_else(|| AppError::not_found("Attachment does not exist!"))?;

    let real_path = expand_tilde(file_path);
    if !real_path.exists() {
        return Err(AppError::not_found("Attachment does not exist on disk!"));
    }

    let live_path = get_live_photo_path(file_path)
        .ok_or_else(|| AppError::not_found("Live photo does not exist for this attachment!"))?;

    stream_file(&live_path, "video/quicktime").await
}

/// GET /api/v1/attachment/{guid}/blurhash
pub async fn blurhash(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let attachment = state
        .imessage_repo
        .lock()
        .get_attachment(&guid)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::not_found("Attachment does not exist!"))?;

    let file_path = attachment
        .filename
        .as_deref()
        .ok_or_else(|| AppError::server_error("Attachment has no file path!"))?;

    let real_path = expand_tilde(file_path);
    if !real_path.exists() {
        return Err(AppError::server_error("Attachment does not exist on disk!"));
    }

    // Check MIME type is an image
    let mime_type = attachment
        .mime_type
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(attachment.uti.as_deref())
        .unwrap_or("");
    if !mime_type.starts_with("image") && !mime_type.starts_with("public.") {
        return Err(AppError::bad_request(
            "Blurhash can only be generated for image attachments",
        ));
    }

    let cache_guid = attachment
        .original_guid
        .as_deref()
        .unwrap_or(&attachment.guid);

    // Check blurhash cache first
    let cache_dir = AppPaths::convert_dir().join(cache_guid);
    let cache_file = cache_dir.join("blurhash.txt");
    if let Ok(cached) = std::fs::read_to_string(&cache_file)
        && !cached.is_empty()
    {
        return Ok(Json(success_response(json!({ "blurhash": cached }))));
    }

    // For HEIC/HEIF/TIFF, convert to JPEG first (reuses existing sips conversion + cache)
    let uti = attachment.uti.as_deref().unwrap_or("");
    let mut img_path = real_path.clone();
    if uti == "public.heic"
        || uti == "public.heif"
        || uti == "public.tiff"
        || mime_type.starts_with("image/heic")
        || mime_type.starts_with("image/heif")
        || mime_type.starts_with("image/tiff")
    {
        let transfer_name = attachment.transfer_name.as_deref().unwrap_or("converted");
        let convert_dir = AppPaths::convert_dir().join(cache_guid);
        let output = convert_dir.join(format!("{transfer_name}.jpeg"));
        if output.exists()
            || (std::fs::create_dir_all(&convert_dir).is_ok()
                && imessage_apple::process::convert_to_jpg(file_path, &output.to_string_lossy())
                    .await
                    .is_ok())
        {
            img_path = output;
        }
    }

    // Use sips to create a small 32x32 thumbnail for fast blurhash computation
    let thumb_dir = AppPaths::convert_dir().join(cache_guid);
    let thumb_path = thumb_dir.join("blurhash_thumb.jpeg");
    if !thumb_path.exists() {
        std::fs::create_dir_all(&thumb_dir)
            .map_err(|e| AppError::server_error(&format!("Failed to create cache dir: {e}")))?;
        imessage_apple::process::resize_image(
            &img_path.to_string_lossy(),
            &thumb_path.to_string_lossy(),
            Some(32),
            None,
        )
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to create thumbnail: {e}")))?;
    }

    // Decode the small thumbnail and compute blurhash
    let img = image::open(&thumb_path)
        .map_err(|e| AppError::server_error(&format!("Failed to decode image: {e}")))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();

    let hash = blurhash::encode(4, 3, w, h, rgba.as_raw())
        .map_err(|e| AppError::server_error(&format!("Blurhash encoding failed: {e}")))?;

    // Cache the result
    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = std::fs::write(&cache_file, &hash);

    Ok(Json(success_response(json!({ "blurhash": hash }))))
}

/// POST /api/v1/attachment/upload
pub async fn upload(mut multipart: Multipart) -> Result<Json<Value>, AppError> {
    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "attachment" {
            file_name = field.file_name().map(|s| s.to_string());
            file_data = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| AppError::server_error(&format!("Failed to read file: {e}")))?
                    .to_vec(),
            );
        }
    }

    let data = file_data.ok_or_else(|| AppError::bad_request("No attachment file provided"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("Attachment file is empty"));
    }
    let name = sanitize_filename(file_name.as_deref().unwrap_or("upload"), "upload");

    // Save to ~/Library/Messages/Attachments/imessage-rs/
    let uuid_str = uuid::Uuid::new_v4().to_string();
    let dir = AppPaths::messages_attachments_dir().join(&uuid_str);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::server_error(&format!("Failed to create directory: {e}")))?;

    let dest = dir.join(&name);
    std::fs::write(&dest, &data)
        .map_err(|e| AppError::server_error(&format!("Failed to write file: {e}")))?;

    let relative_path = PathBuf::from(&uuid_str).join(&name);
    let path_str = relative_path.to_string_lossy().to_string();

    Ok(Json(success_response(json!({ "path": path_str }))))
}

/// Stream a file as an HTTP response with proper headers.
async fn stream_file(
    path: &std::path::Path,
    mime_type: &str,
) -> Result<([(axum::http::HeaderName, String); 3], axum::body::Body), AppError> {
    let file = File::open(path)
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to open file: {e}")))?;

    let metadata = file
        .metadata()
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to read file metadata: {e}")))?;

    let file_name = sanitize_header_filename(
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment"),
        "attachment",
    );

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Ok((
        [
            (header::CONTENT_TYPE, mime_type.to_string()),
            (header::CONTENT_LENGTH, metadata.len().to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
        ],
        body,
    ))
}
