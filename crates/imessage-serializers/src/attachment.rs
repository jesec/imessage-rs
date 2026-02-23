/// Attachment serializer — converts an Attachment entity to the API JSON shape.
///
/// Full response (non-notification):
/// {
///     "originalROWID": 1,
///     "guid": "...",
///     "uti": "public.jpeg",
///     "mimeType": "image/jpeg",
///     "transferName": "photo.jpg",
///     "totalBytes": 123456,
///     "transferState": 5,
///     "isOutgoing": false,
///     "hideAttachment": false,
///     "isSticker": false,
///     "originalGuid": "...",
///     "hasLivePhoto": false,
///     "height": 1920,
///     "width": 1080,
///     "metadata": null
/// }
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Map, Value, json};
use tracing::debug;

use imessage_db::imessage::entities::Attachment;

use crate::config::AttachmentSerializerConfig;

/// Live photo extensions that can have a .mov companion
const LIVE_PHOTO_EXTS: &[&str] = &["png", "jpeg", "jpg", "heic", "tiff"];

/// Resolve ~ to the user's home directory.
fn resolve_tilde(file_path: &str) -> PathBuf {
    if file_path.starts_with('~') {
        let home = home::home_dir().unwrap_or_default();
        home.join(&file_path[2..])
    } else {
        PathBuf::from(file_path)
    }
}

/// Check if a live photo .mov companion exists for the given image path.
fn has_live_photo(file_path: &str) -> bool {
    let real_path = resolve_tilde(file_path);
    let file_str = real_path.to_string_lossy().to_string();

    let ext = if file_str.contains(".heic.jpeg") {
        "heic.jpeg".to_string()
    } else {
        match real_path.extension().and_then(|e| e.to_str()) {
            Some(e) => e.to_lowercase(),
            None => return false,
        }
    };

    let ext_lower = ext.split('.').next_back().unwrap_or(&ext);
    if !LIVE_PHOTO_EXTS.contains(&ext_lower) {
        return false;
    }

    let mov_path = if ext.contains('.') {
        match file_str.strip_suffix(&format!(".{ext}")) {
            Some(stem) => PathBuf::from(format!("{stem}.mov")),
            None => return false,
        }
    } else {
        real_path.with_extension("mov")
    };

    mov_path.exists()
}

/// Extract height/width from attribution_info blob (plist).
/// The blob contains `pgensh` (height) and `pgensw` (width) keys.
fn extract_dimensions_from_attribution_info(blob: &[u8]) -> Option<(u32, u32)> {
    // Try to parse as plist
    let value: plist::Value = plist::from_bytes(blob).ok()?;
    let dict = value.as_dictionary()?;

    let height = dict
        .get("pgensh")
        .and_then(|v| {
            v.as_unsigned_integer()
                .or_else(|| v.as_signed_integer().map(|i| i as u64))
        })
        .map(|v| v as u32)
        .unwrap_or(0);
    let width = dict
        .get("pgensw")
        .and_then(|v| {
            v.as_unsigned_integer()
                .or_else(|| v.as_signed_integer().map(|i| i as u64))
        })
        .map(|v| v as u32)
        .unwrap_or(0);

    if height > 0 || width > 0 {
        Some((width, height))
    } else {
        None
    }
}

/// Image MIME types that support sips dimension extraction.
const SIPS_IMAGE_MIMES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/tiff",
    "image/bmp",
    "image/heic",
    "image/heif",
];

/// Run `mdls` on a file and parse key-value output into a HashMap.
fn get_file_metadata(file_path: &str) -> HashMap<String, String> {
    let real_path = resolve_tilde(file_path);
    if !real_path.exists() {
        return HashMap::new();
    }

    let output = match Command::new("/usr/bin/mdls").arg(&real_path).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return HashMap::new(),
    };

    parse_mdls_output(&output)
}

/// Parse mdls output into key-value pairs.
/// Format: `kMDItemKey = value` or `kMDItemKey = (null)`
fn parse_mdls_output(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some((key, val)) = line.split_once(" = ") {
            let key = key.trim();
            let val = val.trim().trim_matches('"');
            if val != "(null)" {
                map.insert(key.to_string(), val.to_string());
            }
        }
    }
    map
}

/// Build the metadata JSON object from mdls data.
fn build_metadata_object(mdls: &HashMap<String, String>, mime_type: &str) -> Value {
    let mut meta = Map::new();

    if mime_type.starts_with("image/") {
        // Image metadata
        if let Some(v) = mdls
            .get("kMDItemAltitude")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("altitude".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemAperture")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("aperture".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemBitsPerSample")
            .and_then(|s| s.parse::<u32>().ok())
        {
            meta.insert("bitsPerSample".into(), json!(v));
        }
        if let Some(v) = mdls.get("kMDItemColorSpace") {
            meta.insert("colorSpace".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemExposureTimeSeconds")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("exposureTimeSeconds".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemFlashOnOff")
            .and_then(|s| s.parse::<i32>().ok())
        {
            meta.insert("withFlash".into(), json!(v != 0));
        }
        if let Some(v) = mdls
            .get("kMDItemFocalLength")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("focalLength".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemFSSize")
            .and_then(|s| s.parse::<i64>().ok())
        {
            meta.insert("size".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemLatitude")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("latitude".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemLongitude")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("longitude".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemOrientation")
            .and_then(|s| s.parse::<u32>().ok())
        {
            meta.insert("orientation".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemPixelHeight")
            .and_then(|s| s.parse::<u32>().ok())
        {
            meta.insert("height".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemPixelWidth")
            .and_then(|s| s.parse::<u32>().ok())
        {
            meta.insert("width".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemPixelCount")
            .and_then(|s| s.parse::<i64>().ok())
        {
            meta.insert("pixelCount".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemRedEyeOnOff")
            .and_then(|s| s.parse::<i32>().ok())
        {
            meta.insert("withRedEye".into(), json!(v != 0));
        }
        if let Some(v) = mdls
            .get("kMDItemResolutionHeightDPI")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("heightDpi".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemResolutionWidthDPI")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("widthDpi".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemWhiteBalance")
            .and_then(|s| s.parse::<i32>().ok())
        {
            meta.insert("withWhiteBalance".into(), json!(v != 0));
        }
        if let Some(v) = mdls.get("kMDItemProfileName") {
            meta.insert("profileName".into(), json!(v));
        }
        if let Some(v) = mdls.get("kMDItemAcquisitionMake") {
            meta.insert("deviceMake".into(), json!(v));
        }
        if let Some(v) = mdls.get("kMDItemAcquisitionModel") {
            meta.insert("deviceModel".into(), json!(v));
        }
    } else if mime_type.starts_with("video/") {
        if let Some(v) = mdls
            .get("kMDItemFSSize")
            .and_then(|s| s.parse::<i64>().ok())
        {
            meta.insert("bytes".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemPixelHeight")
            .and_then(|s| s.parse::<u32>().ok())
        {
            meta.insert("height".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemPixelWidth")
            .and_then(|s| s.parse::<u32>().ok())
        {
            meta.insert("width".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemAudioBitRate")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("audioBitRate".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemVideoBitRate")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("videoBitRate".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemDurationSeconds")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("duration".into(), json!(v));
        }
        if let Some(v) = mdls.get("kMDItemProfileName") {
            meta.insert("profileName".into(), json!(v));
        }
    } else if mime_type.starts_with("audio/") {
        if let Some(v) = mdls
            .get("kMDItemFSSize")
            .and_then(|s| s.parse::<i64>().ok())
        {
            meta.insert("bytes".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemAudioBitRate")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("bitRate".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemAudioSampleRate")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("sampleRate".into(), json!(v));
        }
        if let Some(v) = mdls
            .get("kMDItemDurationSeconds")
            .and_then(|s| s.parse::<f64>().ok())
        {
            meta.insert("duration".into(), json!(v));
        }
    }

    if meta.is_empty() {
        Value::Null
    } else {
        Value::Object(meta)
    }
}

/// Get image dimensions via sips (fallback when mdls and attribution_info lack data).
fn get_sips_dimensions(file_path: &str) -> Option<(u32, u32)> {
    let real_path = resolve_tilde(file_path);
    if !real_path.exists() {
        return None;
    }

    let output = Command::new("/usr/bin/sips")
        .args([
            "--getProperty",
            "pixelWidth",
            "--getProperty",
            "pixelHeight",
        ])
        .arg(&real_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("pixelWidth:") {
            width = rest.trim().parse().ok();
        } else if let Some(rest) = trimmed.strip_prefix("pixelHeight:") {
            height = rest.trim().parse().ok();
        }
    }

    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    }
}

/// Serialize a single Attachment to JSON.
pub fn serialize_attachment(
    attachment: &Attachment,
    config: &AttachmentSerializerConfig,
    is_for_notification: bool,
) -> Value {
    let mut map = Map::new();

    // Core fields (always present)
    map.insert("originalROWID".to_string(), json!(attachment.rowid));
    map.insert("guid".to_string(), json!(attachment.guid));
    map.insert("uti".to_string(), json!(attachment.uti));
    map.insert("mimeType".to_string(), json!(attachment.mime_type));
    map.insert("transferName".to_string(), json!(attachment.transfer_name));
    map.insert("totalBytes".to_string(), json!(attachment.total_bytes));

    // Non-notification fields
    if !is_for_notification {
        map.insert(
            "transferState".to_string(),
            json!(attachment.transfer_state),
        );
        map.insert("isOutgoing".to_string(), json!(attachment.is_outgoing));
        map.insert(
            "hideAttachment".to_string(),
            json!(attachment.hide_attachment.unwrap_or(false)),
        );
        map.insert(
            "isSticker".to_string(),
            json!(attachment.is_sticker.unwrap_or(false)),
        );
        map.insert("originalGuid".to_string(), json!(attachment.original_guid));

        // hasLivePhoto: check if .mov companion exists on disk
        let live = attachment
            .filename
            .as_deref()
            .map(has_live_photo)
            .unwrap_or(false);
        map.insert("hasLivePhoto".to_string(), json!(live));
    }

    // Metadata (height/width/metadata)
    if config.load_metadata {
        let file_path = attachment.filename.as_deref().unwrap_or("");
        let mime_type = attachment.mime_type.as_deref().unwrap_or("");

        // Get mdls metadata (provides rich info + dimensions)
        let mdls = if !file_path.is_empty() {
            get_file_metadata(file_path)
        } else {
            HashMap::new()
        };

        // Tier 1: mdls dimensions
        let mut width = mdls
            .get("kMDItemPixelWidth")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let mut height = mdls
            .get("kMDItemPixelHeight")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);

        // Tier 2: attribution_info dimensions (override mdls if present)
        if let Some((aw, ah)) = attachment
            .attribution_info
            .as_deref()
            .and_then(extract_dimensions_from_attribution_info)
        {
            width = aw;
            height = ah;
        }

        // Tier 3: sips fallback for images (when no dimensions from mdls or attribution_info)
        if (width == 0 || height == 0)
            && !file_path.is_empty()
            && SIPS_IMAGE_MIMES.contains(&mime_type)
        {
            debug!("Image metadata empty, getting size from sips...");
            if let Some((sw, sh)) = get_sips_dimensions(file_path) {
                width = sw;
                height = sh;
            }
        }

        map.insert("height".to_string(), json!(height));
        map.insert("width".to_string(), json!(width));

        // Build rich metadata object from mdls data
        let metadata = if !mdls.is_empty() {
            build_metadata_object(&mdls, mime_type)
        } else {
            Value::Null
        };
        map.insert("metadata".to_string(), metadata);
    }

    // Data (base64 encoded file contents) — loaded in the HTTP layer
    if config.load_data {
        map.insert("data".to_string(), Value::Null);
    }

    Value::Object(map)
}

/// Serialize a list of Attachments to JSON.
pub fn serialize_attachments(
    attachments: &[Attachment],
    config: &AttachmentSerializerConfig,
    is_for_notification: bool,
) -> Value {
    let list: Vec<Value> = attachments
        .iter()
        .map(|a| serialize_attachment(a, config, is_for_notification))
        .collect();
    Value::Array(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_attachment() -> Attachment {
        Attachment {
            rowid: 10,
            guid: "att-guid-1234".to_string(),
            uti: Some("public.jpeg".to_string()),
            mime_type: Some("image/jpeg".to_string()),
            transfer_name: Some("photo.jpg".to_string()),
            total_bytes: 123456,
            transfer_state: 5,
            is_outgoing: false,
            hide_attachment: Some(false),
            is_sticker: Some(false),
            original_guid: Some("original-guid-1234".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn serialize_full_attachment() {
        let config = AttachmentSerializerConfig::default();
        let json = serialize_attachment(&test_attachment(), &config, false);
        assert_eq!(json["originalROWID"], 10);
        assert_eq!(json["guid"], "att-guid-1234");
        assert_eq!(json["uti"], "public.jpeg");
        assert_eq!(json["mimeType"], "image/jpeg");
        assert_eq!(json["transferName"], "photo.jpg");
        assert_eq!(json["totalBytes"], 123456);
        assert_eq!(json["transferState"], 5);
        assert_eq!(json["isOutgoing"], false);
        assert_eq!(json["isSticker"], false);
        assert_eq!(json["hasLivePhoto"], false);
    }

    #[test]
    fn serialize_notification_attachment() {
        let config = AttachmentSerializerConfig::default();
        let json = serialize_attachment(&test_attachment(), &config, true);
        assert_eq!(json["originalROWID"], 10);
        assert!(json.get("transferState").is_none());
        assert!(json.get("isOutgoing").is_none());
        assert!(json.get("hasLivePhoto").is_none());
    }

    #[test]
    fn metadata_included_by_default() {
        let config = AttachmentSerializerConfig::default();
        let json = serialize_attachment(&test_attachment(), &config, false);
        assert!(json.get("height").is_some());
        assert!(json.get("width").is_some());
    }

    #[test]
    fn data_excluded_by_default() {
        let config = AttachmentSerializerConfig::default();
        let json = serialize_attachment(&test_attachment(), &config, false);
        assert!(json.get("data").is_none());
    }

    #[test]
    fn parse_mdls_output_basic() {
        let output = "kMDItemPixelHeight = 1920\nkMDItemPixelWidth  = 1080\nkMDItemFSSize      = 123456\nkMDItemColorSpace  = \"sRGB\"\n";
        let map = parse_mdls_output(output);
        assert_eq!(map.get("kMDItemPixelHeight").unwrap(), "1920");
        assert_eq!(map.get("kMDItemPixelWidth").unwrap(), "1080");
        assert_eq!(map.get("kMDItemFSSize").unwrap(), "123456");
        assert_eq!(map.get("kMDItemColorSpace").unwrap(), "sRGB");
    }

    #[test]
    fn parse_mdls_output_null_skipped() {
        let output = "kMDItemLatitude = (null)\nkMDItemPixelWidth = 500\n";
        let map = parse_mdls_output(output);
        assert!(!map.contains_key("kMDItemLatitude"));
        assert_eq!(map.get("kMDItemPixelWidth").unwrap(), "500");
    }

    #[test]
    fn build_metadata_image() {
        let mut mdls = HashMap::new();
        mdls.insert("kMDItemPixelHeight".into(), "1920".into());
        mdls.insert("kMDItemPixelWidth".into(), "1080".into());
        mdls.insert("kMDItemLatitude".into(), "37.7749".into());
        mdls.insert("kMDItemLongitude".into(), "-122.4194".into());
        let meta = build_metadata_object(&mdls, "image/jpeg");
        assert_eq!(meta["height"], 1920);
        assert_eq!(meta["width"], 1080);
        assert!((meta["latitude"].as_f64().unwrap() - 37.7749).abs() < 0.001);
    }

    #[test]
    fn build_metadata_video() {
        let mut mdls = HashMap::new();
        mdls.insert("kMDItemPixelHeight".into(), "720".into());
        mdls.insert("kMDItemPixelWidth".into(), "1280".into());
        mdls.insert("kMDItemDurationSeconds".into(), "30.5".into());
        let meta = build_metadata_object(&mdls, "video/mp4");
        assert_eq!(meta["height"], 720);
        assert_eq!(meta["width"], 1280);
        assert!((meta["duration"].as_f64().unwrap() - 30.5).abs() < 0.01);
    }

    #[test]
    fn build_metadata_audio() {
        let mut mdls = HashMap::new();
        mdls.insert("kMDItemDurationSeconds".into(), "180.0".into());
        mdls.insert("kMDItemAudioBitRate".into(), "128000".into());
        let meta = build_metadata_object(&mdls, "audio/mpeg");
        assert!((meta["duration"].as_f64().unwrap() - 180.0).abs() < 0.01);
        assert!((meta["bitRate"].as_f64().unwrap() - 128000.0).abs() < 0.01);
    }

    #[test]
    fn build_metadata_empty_returns_null() {
        let mdls = HashMap::new();
        let meta = build_metadata_object(&mdls, "image/jpeg");
        assert!(meta.is_null());
    }
}
