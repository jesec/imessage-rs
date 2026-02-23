//! Apple date conversion utilities.
//!
//! The iMessage database (chat.db) stores dates as Apple timestamps:
//!   - Epoch: January 1, 2001 00:00:00 UTC
//!   - Unit: nanoseconds (on High Sierra+, multiplier = 10^9) stored as i64
//!   - MULTIPLIER = 10^6 is used for the conversion, treating the raw value
//!     as microseconds.
//!
//! The formula:
//!   unix_ms = APPLE_EPOCH_MS + (raw_value / MULTIPLIER)
//!
//! Where:
//!   APPLE_EPOCH_MS = 978307200000 (Jan 1, 2001 in Unix ms)
//!   MULTIPLIER = 1_000_000

/// Unix timestamp (ms) of January 1, 2001 00:00:00 UTC.
pub const APPLE_EPOCH_MS: i64 = 978_307_200_000;

/// The multiplier used in the iMessage database (High Sierra+).
/// Raw DB value / MULTIPLIER = milliseconds offset from Apple epoch.
pub const MULTIPLIER: i64 = 1_000_000;

/// Convert a raw Apple timestamp from chat.db to Unix epoch milliseconds.
///
/// Returns `None` if the raw value is 0 or negative (meaning "not set").
pub fn apple_to_unix_ms(raw: i64) -> Option<i64> {
    if raw <= 0 {
        return None;
    }
    Some(APPLE_EPOCH_MS + (raw / MULTIPLIER))
}

/// Convert Unix epoch milliseconds to a raw Apple timestamp for chat.db queries.
pub fn unix_ms_to_apple(ms: i64) -> i64 {
    (ms - APPLE_EPOCH_MS) * MULTIPLIER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_epoch_is_correct() {
        // Jan 1, 2001 00:00:00 UTC = 978307200 seconds since Unix epoch
        assert_eq!(APPLE_EPOCH_MS, 978_307_200_000);
    }

    #[test]
    fn zero_returns_none() {
        assert_eq!(apple_to_unix_ms(0), None);
    }

    #[test]
    fn negative_returns_none() {
        assert_eq!(apple_to_unix_ms(-1), None);
    }

    #[test]
    fn one_second_after_apple_epoch() {
        // 1 second = 1000ms. In DB: 1000 * MULTIPLIER = 1_000_000_000
        let raw = 1000 * MULTIPLIER;
        let result = apple_to_unix_ms(raw).unwrap();
        assert_eq!(result, APPLE_EPOCH_MS + 1000);
    }

    #[test]
    fn ten_seconds_after_apple_epoch() {
        let raw = 10_000 * MULTIPLIER;
        let result = apple_to_unix_ms(raw).unwrap();
        assert_eq!(result, APPLE_EPOCH_MS + 10_000);
    }

    #[test]
    fn roundtrip() {
        let now_ms: i64 = 1_700_000_000_000; // ~Nov 2023
        let apple = unix_ms_to_apple(now_ms);
        let back = apple_to_unix_ms(apple).unwrap();
        assert_eq!(back, now_ms);
    }

    #[test]
    fn real_message_date() {
        // Live captured from macOS 26.3 Tahoe: dateEdited: 1771445533049 (Unix ms)
        // This is a Unix ms timestamp returned by the serializer, not a raw DB value.
        // Verify that converting back and forth preserves it.
        let unix_ms: i64 = 1_771_445_533_049;
        let raw = unix_ms_to_apple(unix_ms);
        assert!(raw > 0);
        let back = apple_to_unix_ms(raw).unwrap();
        assert_eq!(back, unix_ms);
    }

    #[test]
    fn multiplier_value() {
        assert_eq!(MULTIPLIER, 1_000_000);
    }

    #[test]
    fn sub_millisecond_truncates() {
        // Raw values not exactly divisible by MULTIPLIER have sub-ms precision.
        // Integer division truncates (floors) the fractional ms, which matches
        // JavaScript's Date constructor behavior (Date truncates fractional ms).
        let raw: i64 = 793_138_336_556_999_936;
        let result = apple_to_unix_ms(raw).unwrap();
        // 793138336556999936 / 1000000 = 793138336556 (truncated)
        assert_eq!(result, APPLE_EPOCH_MS + 793_138_336_556);
    }

    #[test]
    fn real_db_values_match_node() {
        // Verified against live chat.db raw values.
        let cases: Vec<(i64, i64)> = vec![
            (793_138_336_352_743_000, 1_771_445_536_352),
            (793_138_336_557_000_000, 1_771_445_536_557),
            (793_138_338_791_721_000, 1_771_445_538_791),
            (793_138_267_580_708_000, 1_771_445_467_580),
            (793_138_269_954_512_000, 1_771_445_469_954),
        ];
        for (raw, expected) in cases {
            let result = apple_to_unix_ms(raw).unwrap();
            assert_eq!(result, expected, "raw={raw}");
        }
    }
}
