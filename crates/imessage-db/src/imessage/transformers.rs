/// Value transformers that convert raw SQLite values to Rust types.
///
/// - BooleanTransformer: integer 0/1 → bool
/// - DateTransformer: Apple timestamp → Unix ms (Option<i64>)
/// - ReactionTypeTransformer: reaction integer → string name
use imessage_core::dates::apple_to_unix_ms;

/// Convert a SQLite integer (0/1) to a boolean.
pub fn bool_from_db(val: i64) -> bool {
    val != 0
}

/// Convert an Apple timestamp from the iMessage DB to Unix epoch milliseconds.
/// Returns `None` if the value is 0 or null (meaning "not set").
pub fn date_from_db(val: i64) -> Option<i64> {
    apple_to_unix_ms(val)
}

/// Convert Unix epoch milliseconds to an Apple timestamp for DB queries.
pub fn date_to_db(unix_ms: i64) -> i64 {
    imessage_core::dates::unix_ms_to_apple(unix_ms)
}

/// Map of reaction integer IDs to their string names.
pub fn reaction_type_from_db(val: i64) -> Option<String> {
    match val {
        0 => None,
        1000 => Some("sticker".to_string()),
        2000 => Some("love".to_string()),
        2001 => Some("like".to_string()),
        2002 => Some("dislike".to_string()),
        2003 => Some("laugh".to_string()),
        2004 => Some("emphasize".to_string()),
        2005 => Some("question".to_string()),
        2006 => Some("emoji".to_string()),
        2007 => Some("sticker-tapback".to_string()),
        3000 => Some("-love".to_string()),
        3001 => Some("-like".to_string()),
        3002 => Some("-dislike".to_string()),
        3003 => Some("-laugh".to_string()),
        3004 => Some("-emphasize".to_string()),
        3005 => Some("-question".to_string()),
        3006 => Some("-emoji".to_string()),
        3007 => Some("-sticker-tapback".to_string()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_from_db_values() {
        assert!(!bool_from_db(0));
        assert!(bool_from_db(1));
        assert!(bool_from_db(42)); // any non-zero is true
    }

    #[test]
    fn date_transform_zero_is_none() {
        assert_eq!(date_from_db(0), None);
    }

    #[test]
    fn date_transform_positive_value() {
        // 1 second after Apple epoch = APPLE_EPOCH_MS + 1000
        let raw = 1000 * 1_000_000; // 1000ms * multiplier
        let result = date_from_db(raw);
        assert_eq!(result, Some(978_307_200_000 + 1000));
    }

    #[test]
    fn reaction_type_known_values() {
        assert_eq!(reaction_type_from_db(0), None);
        assert_eq!(reaction_type_from_db(2000), Some("love".to_string()));
        assert_eq!(reaction_type_from_db(2001), Some("like".to_string()));
        assert_eq!(reaction_type_from_db(3000), Some("-love".to_string()));
    }

    #[test]
    fn reaction_type_emoji_and_sticker() {
        assert_eq!(reaction_type_from_db(2006), Some("emoji".to_string()));
        assert_eq!(
            reaction_type_from_db(2007),
            Some("sticker-tapback".to_string())
        );
        assert_eq!(reaction_type_from_db(3006), Some("-emoji".to_string()));
        assert_eq!(
            reaction_type_from_db(3007),
            Some("-sticker-tapback".to_string())
        );
    }

    #[test]
    fn reaction_type_unknown_passthrough() {
        assert_eq!(reaction_type_from_db(9999), Some("9999".to_string()));
    }
}
