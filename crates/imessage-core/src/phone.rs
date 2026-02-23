/// Phone number and address normalization utilities.
use std::str::FromStr;

/// Normalize an address (phone number or email) to a canonical form.
/// - Emails: lowercased
/// - Phone numbers: E.164 format via the `phonenumber` crate
/// - Falls back to original string on parse failure
pub fn normalize_address(address: &str, region: &str) -> String {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }

    // Emails: just lowercase
    if trimmed.contains('@') {
        return trimmed.to_lowercase();
    }

    // Phone numbers: parse with phonenumber crate, format E.164
    let region_code = if region.is_empty() { "US" } else { region };
    let country_id = phonenumber::country::Id::from_str(region_code).ok();

    match phonenumber::parse(country_id, trimmed) {
        Ok(number) => {
            if phonenumber::is_valid(&number) {
                phonenumber::format(&number)
                    .mode(phonenumber::Mode::E164)
                    .to_string()
            } else {
                trimmed.to_string()
            }
        }
        Err(_) => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_email() {
        assert_eq!(normalize_address("Foo@Bar.com", "US"), "foo@bar.com");
    }

    #[test]
    fn normalize_us_phone() {
        // Use a valid US number (202 = Washington DC area code)
        assert_eq!(normalize_address("+1 (202) 555-0100", "US"), "+12025550100");
    }

    #[test]
    fn normalize_us_phone_no_country_code() {
        assert_eq!(normalize_address("(202) 555-0100", "US"), "+12025550100");
    }

    #[test]
    fn normalize_already_e164() {
        assert_eq!(normalize_address("+12025550100", "US"), "+12025550100");
    }

    #[test]
    fn normalize_invalid_phone_falls_back() {
        // 555-0199 range is reserved/invalid, so phonenumber may not validate it
        // Simple invalid numbers should fall back to original
        assert_eq!(normalize_address("12345", "US"), "12345");
    }

    #[test]
    fn normalize_invalid_falls_back() {
        assert_eq!(normalize_address("not-a-number", "US"), "not-a-number");
    }

    #[test]
    fn normalize_empty() {
        assert_eq!(normalize_address("", "US"), "");
    }
}
