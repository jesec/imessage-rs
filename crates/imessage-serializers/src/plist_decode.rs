//! Binary plist decoder for chat.db message_summary_info and payload_data blobs.
//!
//! These columns contain binary plists (NOT typedstream). This module parses
//! them and applies post-processing (key renaming, dict unwrapping):
//! - Short key renaming (e.g., "rp" → "retractedParts")
//! - Single-element dict unwrapping ({0: x} → x)
//! - Range object conversion ({lo, le} → [lo, le])
//! - Recursive blob decoding (nested Data values → try typedstream/plist)

use serde_json::{Map, Value, json};

use imessage_core::typedstream;

/// Short key → full key mapping.
fn rename_key(key: &str) -> &str {
    match key {
        "ec" => "editedContent",
        "ep" => "editedParts",
        "rp" => "retractedParts",
        "euh" => "editingUserHandle",
        "bcg" => "backwardsCompatibilityGuid",
        "d" => "date",
        "t" => "text",
        "otr" => "originalTextRange",
        "amc" => "associatedMessageContent",
        "ams" => "associatedMessageSummary",
        other => other,
    }
}

/// Convert a `plist::Value` to `serde_json::Value` with post-processing.
fn plist_to_json(val: &plist::Value) -> Value {
    match val {
        plist::Value::Array(arr) => Value::Array(arr.iter().map(plist_to_json).collect()),
        plist::Value::Dictionary(dict) => {
            let keys: Vec<&String> = dict.keys().collect();

            // If dict has key '0' but not '1', unwrap the single entry
            if keys.len() == 1 && keys[0] == "0" {
                return plist_to_json(dict.get("0").unwrap());
            }

            // Special case: range object {lo, le} with exactly 2 keys → [lo, le]
            if keys.len() == 2 && dict.contains_key("lo") && dict.contains_key("le") {
                return json!([
                    plist_to_json(dict.get("lo").unwrap()),
                    plist_to_json(dict.get("le").unwrap()),
                ]);
            }

            // Normal dict: rename keys and recurse
            let mut map = Map::new();
            for (k, v) in dict.iter() {
                let key = rename_key(k).to_string();
                map.insert(key, plist_to_json(v));
            }
            Value::Object(map)
        }
        plist::Value::Boolean(b) => json!(b),
        plist::Value::Data(data) => {
            // Try recursive decode: typedstream first, then plist
            if let Some(decoded) = typedstream::decode_attributed_body(data) {
                // If single-element array, unwrap
                if let Value::Array(arr) = &decoded
                    && arr.len() == 1
                {
                    return arr[0].clone();
                }
                return decoded;
            }
            if let Some(decoded) = decode_message_plist(data) {
                return decoded;
            }
            // Can't decode — return null
            Value::Null
        }
        plist::Value::Date(d) => json!(d.to_xml_format()),
        plist::Value::Real(f) => json!(f),
        plist::Value::Integer(i) => {
            if let Some(n) = i.as_signed() {
                json!(n)
            } else if let Some(n) = i.as_unsigned() {
                json!(n)
            } else {
                Value::Null
            }
        }
        plist::Value::String(s) => json!(s),
        plist::Value::Uid(u) => json!(u.get()),
        _ => Value::Null,
    }
}

/// Decode a binary plist blob (message_summary_info or payload_data) to JSON.
///
/// Applies post-processing: key renaming, dict unwrapping,
/// range conversion, and recursive blob decoding.
///
/// Returns `None` if the blob is not a valid binary plist.
pub fn decode_message_plist(data: &[u8]) -> Option<Value> {
    if data.is_empty() {
        return None;
    }

    let plist_val: plist::Value = plist::from_bytes(data).ok()?;
    Some(plist_to_json(&plist_val))
}

/// Decode a binary plist blob (chat.properties) to JSON without message-specific post-processing.
///
/// Unlike `decode_message_plist`, this does NOT apply key renaming, range conversion,
/// or recursive blob decoding. It just does a raw plist → JSON conversion.
pub fn decode_chat_properties(data: &[u8]) -> Option<Value> {
    if data.is_empty() {
        return None;
    }
    let plist_val: plist::Value = plist::from_bytes(data).ok()?;
    Some(plist_to_json_raw(&plist_val))
}

/// Raw plist to JSON conversion without message-specific post-processing.
fn plist_to_json_raw(val: &plist::Value) -> Value {
    match val {
        plist::Value::Array(arr) => Value::Array(arr.iter().map(plist_to_json_raw).collect()),
        plist::Value::Dictionary(dict) => {
            let mut map = Map::new();
            for (k, v) in dict.iter() {
                map.insert(k.clone(), plist_to_json_raw(v));
            }
            Value::Object(map)
        }
        plist::Value::Boolean(b) => json!(b),
        plist::Value::Data(_) => {
            // Binary data in properties — return null (not user-visible)
            Value::Null
        }
        plist::Value::Date(d) => json!(d.to_xml_format()),
        plist::Value::Real(f) => json!(f),
        plist::Value::Integer(i) => {
            if let Some(n) = i.as_signed() {
                json!(n)
            } else if let Some(n) = i.as_unsigned() {
                json!(n)
            } else {
                Value::Null
            }
        }
        plist::Value::String(s) => json!(s),
        plist::Value::Uid(u) => json!(u.get()),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_known_keys() {
        assert_eq!(rename_key("rp"), "retractedParts");
        assert_eq!(rename_key("ec"), "editedContent");
        assert_eq!(rename_key("ep"), "editedParts");
        assert_eq!(rename_key("d"), "date");
        assert_eq!(rename_key("t"), "text");
        assert_eq!(rename_key("unknown"), "unknown");
    }

    #[test]
    fn empty_data_returns_none() {
        assert!(decode_message_plist(&[]).is_none());
    }

    #[test]
    fn invalid_data_returns_none() {
        assert!(decode_message_plist(b"not a plist").is_none());
    }

    #[test]
    fn decode_simple_plist() {
        // Build a simple binary plist with a dict containing "rp" key
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "rp".to_string(),
            plist::Value::Array(vec![plist::Value::Integer(0.into())]),
        );
        let val = plist::Value::Dictionary(dict);

        let mut buf = Vec::new();
        plist::to_writer_binary(&mut buf, &val).unwrap();

        let result = decode_message_plist(&buf).unwrap();
        // "rp" should be renamed to "retractedParts"
        assert!(result.get("retractedParts").is_some());
        assert_eq!(result["retractedParts"], json!([0]));
    }

    #[test]
    fn single_element_dict_unwrap() {
        // {0: "hello"} should unwrap to "hello"
        let mut dict = plist::Dictionary::new();
        dict.insert("0".to_string(), plist::Value::String("hello".to_string()));
        let val = plist::Value::Dictionary(dict);

        let mut buf = Vec::new();
        plist::to_writer_binary(&mut buf, &val).unwrap();

        let result = decode_message_plist(&buf).unwrap();
        assert_eq!(result, json!("hello"));
    }

    #[test]
    fn range_object_conversion() {
        // {lo: 5, le: 10} should become [5, 10]
        let mut dict = plist::Dictionary::new();
        dict.insert("lo".to_string(), plist::Value::Integer(5.into()));
        dict.insert("le".to_string(), plist::Value::Integer(10.into()));
        let val = plist::Value::Dictionary(dict);

        let mut buf = Vec::new();
        plist::to_writer_binary(&mut buf, &val).unwrap();

        let result = decode_message_plist(&buf).unwrap();
        assert_eq!(result, json!([5, 10]));
    }
}
