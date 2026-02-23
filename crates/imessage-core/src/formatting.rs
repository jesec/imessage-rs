/// Markdown-to-iMessage text formatting parser.
///
/// Converts markdown emphasis markers to iMessage `textFormatting` ranges.
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::LazyLock;

/// A text formatting style supported by iMessage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextFormattingStyle {
    Bold,
    Italic,
    Underline,
    Strikethrough,
}

impl TextFormattingStyle {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::Underline => "underline",
            Self::Strikethrough => "strikethrough",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "bold" => Some(Self::Bold),
            "italic" => Some(Self::Italic),
            "underline" => Some(Self::Underline),
            "strikethrough" => Some(Self::Strikethrough),
            _ => None,
        }
    }
}

/// A formatting range: start offset, length, and styles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFormattingRange {
    pub start: usize,
    pub length: usize,
    pub styles: Vec<TextFormattingStyle>,
}

impl TextFormattingRange {
    pub fn to_json(&self) -> Value {
        json!({
            "start": self.start,
            "length": self.length,
            "styles": self.styles.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        })
    }
}

/// Result of parsing markdown formatting.
pub struct ParsedFormatting {
    pub clean_text: String,
    pub formatting: Vec<TextFormattingRange>,
}

impl ParsedFormatting {
    /// Convert the formatting ranges to a JSON array suitable for the Private API.
    pub fn formatting_json(&self) -> Value {
        json!(
            self.formatting
                .iter()
                .map(|r| r.to_json())
                .collect::<Vec<_>>()
        )
    }
}

/// Validate client-provided text formatting ranges.
pub fn validate_text_formatting(formatting: &Value, message: &str) -> Result<(), String> {
    let arr = formatting
        .as_array()
        .ok_or("textFormatting must be an array")?;

    if message.is_empty() {
        return Err("A non-empty 'message' is required when using textFormatting".to_string());
    }

    let msg_len = message.len();
    for (i, range) in arr.iter().enumerate() {
        let obj = range
            .as_object()
            .ok_or(format!("textFormatting[{i}] must be an object"))?;

        let start = obj
            .get("start")
            .and_then(|v| v.as_u64())
            .ok_or(format!("textFormatting[{i}].start must be an integer >= 0"))?
            as usize;

        let length = obj
            .get("length")
            .and_then(|v| v.as_u64())
            .filter(|&v| v > 0)
            .ok_or(format!("textFormatting[{i}].length must be an integer > 0"))?
            as usize;

        if start + length > msg_len {
            return Err(format!("textFormatting[{i}] range exceeds message length"));
        }

        let styles = obj
            .get("styles")
            .and_then(|v| v.as_array())
            .filter(|a| !a.is_empty())
            .ok_or(format!(
                "textFormatting[{i}].styles must be a non-empty array"
            ))?;

        for style_val in styles {
            let s = style_val.as_str().ok_or(format!(
                "textFormatting[{i}].styles contains non-string value"
            ))?;
            if TextFormattingStyle::from_str(s).is_none() {
                return Err(format!(
                    "textFormatting[{i}].styles contains unsupported value: {s}"
                ));
            }
        }
    }

    Ok(())
}

/// Check if a JSON value represents non-empty text formatting.
pub fn has_text_formatting(formatting: Option<&Value>) -> bool {
    formatting
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
}

// PUA character bases for protecting regions
const PUA_PROTECT: char = '\u{E000}';
const PUA_ESCAPE: char = '\u{F000}';

/// Emphasis pattern definition.
struct EmphasisPattern {
    regex: &'static LazyLock<Regex>,
    styles: Vec<TextFormattingStyle>,
}

// Pre-compiled regex patterns.
// Uses fancy-regex for look-around support.
static RE_FENCED_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"```[\s\S]*?```").unwrap());
static RE_INLINE_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]+`").unwrap());
static RE_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://[^\s)>\]]+").unwrap());
static RE_BACKSLASH_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\\([\\`*_~\{}\[\]<>()\#+\-.!|])"#).unwrap());

// Emphasis patterns (longest markers first)
static RE_BOLD_ITALIC_STAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*\*(.+?)\*\*\*").unwrap());
static RE_BOLD_ITALIC_UNDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?<!\w)___(.+?)___(?!\w)").unwrap());
static RE_BOLD_STAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*(.+?)\*\*").unwrap());
static RE_BOLD_UNDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?<!\w)__(.+?)__(?!\w)").unwrap());
static RE_ITALIC_STAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?<!\*)\*(?!\*)(.+?)(?<!\*)\*(?!\*)").unwrap());
static RE_ITALIC_UNDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?<!\w)_(?!_)(.+?)(?<!_)_(?!\w)").unwrap());
static RE_STRIKETHROUGH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"~~(.+?)~~").unwrap());

/// Collect all non-overlapping matches from a fancy_regex pattern.
fn find_all_matches(re: &Regex, text: &str) -> Vec<(usize, usize, String)> {
    let mut results = Vec::new();
    let mut start = 0;
    while start < text.len() {
        match re.find_from_pos(text, start) {
            Ok(Some(m)) => {
                results.push((m.start(), m.end(), m.as_str().to_string()));
                start = m.end();
            }
            _ => break,
        }
    }
    results
}

/// Collect all captures from a fancy_regex pattern.
fn captures_all(re: &Regex, text: &str) -> Vec<(usize, usize, String, String)> {
    // Returns: (full_start, full_end, full_text, group1_text)
    let mut results = Vec::new();
    let mut start = 0;
    while start < text.len() {
        match re.captures_from_pos(text, start) {
            Ok(Some(caps)) => {
                let full = caps.get(0).unwrap();
                let group1 = caps
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                results.push((full.start(), full.end(), full.as_str().to_string(), group1));
                start = full.end();
            }
            _ => break,
        }
    }
    results
}

/// Parse markdown emphasis markers from message text and return cleaned text
/// plus formatting ranges. Returns `None` if text is unchanged.
///
/// Algorithm (4 phases):
/// 1. Protect code blocks, inline code, URLs with PUA character runs
/// 2. Process backslash escapes
/// 3. Sequential emphasis passes (longest markers first), adjusting offsets
/// 4. Restore protected regions and escaped characters
pub fn parse_markdown_formatting(text: &str) -> Option<ParsedFormatting> {
    if text.is_empty() {
        return None;
    }

    // --- Phase 1: Protect regions that must not be parsed for emphasis. ---
    let mut protected_regions: Vec<String> = Vec::new();
    let mut work = text.to_string();

    let protect = |work: &mut String, regions: &mut Vec<String>, re: &Regex| {
        let matches = find_all_matches(re, work);
        if matches.is_empty() {
            return;
        }
        let mut result = String::new();
        let mut last = 0;
        for (mstart, mend, mtext) in &matches {
            result.push_str(&work[last..*mstart]);
            let idx = regions.len();
            regions.push(mtext.clone());
            let pua = char::from_u32(PUA_PROTECT as u32 + idx as u32).unwrap_or(PUA_PROTECT);
            for _ in 0..mtext.len() {
                result.push(pua);
            }
            last = *mend;
        }
        result.push_str(&work[last..]);
        *work = result;
    };

    protect(&mut work, &mut protected_regions, &RE_FENCED_CODE);
    protect(&mut work, &mut protected_regions, &RE_INLINE_CODE);
    protect(&mut work, &mut protected_regions, &RE_URL);

    // --- Phase 2: Backslash escapes. ---
    let mut escaped_chars: Vec<String> = Vec::new();
    {
        let caps = captures_all(&RE_BACKSLASH_ESCAPE, &work);
        if !caps.is_empty() {
            let mut result = String::new();
            let mut last = 0;
            for (fstart, fend, _, group1) in &caps {
                result.push_str(&work[last..*fstart]);
                let idx = escaped_chars.len();
                escaped_chars.push(group1.clone());
                let pua = char::from_u32(PUA_ESCAPE as u32 + idx as u32).unwrap_or(PUA_ESCAPE);
                result.push(pua);
                last = *fend;
            }
            result.push_str(&work[last..]);
            work = result;
        }
    }

    // --- Phase 3: Sequential emphasis passes (longest markers first). ---
    let mut formatting: Vec<TextFormattingRange> = Vec::new();

    let patterns = [
        EmphasisPattern {
            regex: &RE_BOLD_ITALIC_STAR,
            styles: vec![TextFormattingStyle::Bold, TextFormattingStyle::Italic],
        },
        EmphasisPattern {
            regex: &RE_BOLD_ITALIC_UNDER,
            styles: vec![TextFormattingStyle::Bold, TextFormattingStyle::Italic],
        },
        EmphasisPattern {
            regex: &RE_BOLD_STAR,
            styles: vec![TextFormattingStyle::Bold],
        },
        EmphasisPattern {
            regex: &RE_BOLD_UNDER,
            styles: vec![TextFormattingStyle::Bold],
        },
        EmphasisPattern {
            regex: &RE_ITALIC_STAR,
            styles: vec![TextFormattingStyle::Italic],
        },
        EmphasisPattern {
            regex: &RE_ITALIC_UNDER,
            styles: vec![TextFormattingStyle::Italic],
        },
        EmphasisPattern {
            regex: &RE_STRIKETHROUGH,
            styles: vec![TextFormattingStyle::Strikethrough],
        },
    ];

    for pattern in &patterns {
        let caps_list = captures_all(pattern.regex, &work);
        if caps_list.is_empty() {
            continue;
        }

        let mut pass_ranges: Vec<TextFormattingRange> = Vec::new();
        let mut removed_positions: Vec<usize> = Vec::new();

        let mut result = String::new();
        let mut last = 0;

        for (fstart, fend, full_text, content) in &caps_list {
            result.push_str(&work[last..*fstart]);

            let marker_len = (full_text.len() - content.len()) / 2;

            // Track positions of removed marker chars (in this pass's coordinates)
            for j in 0..marker_len {
                removed_positions.push(fstart + j);
            }
            for j in 0..marker_len {
                removed_positions.push(fstart + marker_len + content.len() + j);
            }

            let start = result.len();
            result.push_str(content);
            pass_ranges.push(TextFormattingRange {
                start,
                length: content.len(),
                styles: pattern.styles.clone(),
            });

            last = *fend;
        }

        result.push_str(&work[last..]);
        work = result;

        // Adjust all previously-computed ranges for markers removed in this pass
        if !removed_positions.is_empty() {
            removed_positions.sort();
            for range in &mut formatting {
                let mut start_shift = 0usize;
                let mut length_reduction = 0usize;
                for &pos in &removed_positions {
                    if pos < range.start {
                        start_shift += 1;
                    } else if pos < range.start + range.length {
                        length_reduction += 1;
                    }
                }
                range.start -= start_shift;
                range.length -= length_reduction;
            }
        }

        formatting.extend(pass_ranges);
    }

    // --- Phase 4: Restore protected regions and escaped characters. ---
    let mut clean_text = work;

    // Code/URL regions (same-length PUA runs -> original text)
    for i in (0..protected_regions.len()).rev() {
        let pua = char::from_u32(PUA_PROTECT as u32 + i as u32).unwrap_or(PUA_PROTECT);
        let pua_run: String = std::iter::repeat_n(pua, protected_regions[i].len()).collect();
        clean_text = clean_text.replace(&pua_run, &protected_regions[i]);
    }

    // Escaped chars (single PUA char -> original literal char)
    for (i, escaped) in escaped_chars.iter().enumerate() {
        let pua = char::from_u32(PUA_ESCAPE as u32 + i as u32).unwrap_or(PUA_ESCAPE);
        clean_text = clean_text.replace(pua, escaped);
    }

    // Nothing changed -> return None
    if formatting.is_empty() && clean_text == text {
        return None;
    }

    // Drop degenerate ranges and sort by position
    formatting.retain(|r| r.length > 0);
    formatting.sort_by_key(|r| r.start);

    Some(ParsedFormatting {
        clean_text,
        formatting,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_empty_input() {
        assert!(parse_markdown_formatting("").is_none());
    }

    #[test]
    fn plain_text_unchanged() {
        assert!(parse_markdown_formatting("hello world").is_none());
    }

    #[test]
    fn bold_stars() {
        let r = parse_markdown_formatting("**bold**").unwrap();
        assert_eq!(r.clean_text, "bold");
        assert_eq!(r.formatting.len(), 1);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 4);
        assert_eq!(r.formatting[0].styles, vec![TextFormattingStyle::Bold]);
    }

    #[test]
    fn italic_stars() {
        let r = parse_markdown_formatting("*italic*").unwrap();
        assert_eq!(r.clean_text, "italic");
        assert_eq!(r.formatting.len(), 1);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 6);
        assert_eq!(r.formatting[0].styles, vec![TextFormattingStyle::Italic]);
    }

    #[test]
    fn strikethrough() {
        let r = parse_markdown_formatting("~~struck~~").unwrap();
        assert_eq!(r.clean_text, "struck");
        assert_eq!(r.formatting.len(), 1);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 6);
        assert_eq!(
            r.formatting[0].styles,
            vec![TextFormattingStyle::Strikethrough]
        );
    }

    #[test]
    fn bold_italic_stars() {
        let r = parse_markdown_formatting("***both***").unwrap();
        assert_eq!(r.clean_text, "both");
        assert_eq!(r.formatting.len(), 1);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 4);
        assert_eq!(
            r.formatting[0].styles,
            vec![TextFormattingStyle::Bold, TextFormattingStyle::Italic]
        );
    }

    #[test]
    fn bold_underscore() {
        let r = parse_markdown_formatting("__bold__").unwrap();
        assert_eq!(r.clean_text, "bold");
        assert_eq!(r.formatting.len(), 1);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 4);
        assert_eq!(r.formatting[0].styles, vec![TextFormattingStyle::Bold]);
    }

    #[test]
    fn italic_underscore() {
        let r = parse_markdown_formatting("_italic_").unwrap();
        assert_eq!(r.clean_text, "italic");
        assert_eq!(r.formatting.len(), 1);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 6);
        assert_eq!(r.formatting[0].styles, vec![TextFormattingStyle::Italic]);
    }

    #[test]
    fn bold_italic_underscore() {
        let r = parse_markdown_formatting("___both___").unwrap();
        assert_eq!(r.clean_text, "both");
        assert_eq!(r.formatting.len(), 1);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 4);
        assert_eq!(
            r.formatting[0].styles,
            vec![TextFormattingStyle::Bold, TextFormattingStyle::Italic]
        );
    }

    #[test]
    fn mid_word_underscores_preserved() {
        assert!(parse_markdown_formatting("some_var_name").is_none());
    }

    #[test]
    fn urls_protected() {
        assert!(
            parse_markdown_formatting("https://en.wikipedia.org/wiki/Hong_Kong_Island").is_none()
        );
    }

    #[test]
    fn code_spans_protected() {
        assert!(parse_markdown_formatting("`*not italic*`").is_none());
    }

    #[test]
    fn backslash_escapes() {
        let r = parse_markdown_formatting("\\*literal\\*").unwrap();
        assert_eq!(r.clean_text, "*literal*");
        assert!(r.formatting.is_empty());
    }

    #[test]
    fn mixed_formatting_correct_offsets() {
        let r = parse_markdown_formatting("**bold** and *italic*").unwrap();
        assert_eq!(r.clean_text, "bold and italic");
        assert_eq!(r.formatting.len(), 2);
        assert_eq!(r.formatting[0].start, 0);
        assert_eq!(r.formatting[0].length, 4);
        assert_eq!(r.formatting[0].styles, vec![TextFormattingStyle::Bold]);
        assert_eq!(r.formatting[1].start, 9);
        assert_eq!(r.formatting[1].length, 6);
        assert_eq!(r.formatting[1].styles, vec![TextFormattingStyle::Italic]);
    }

    #[test]
    fn fenced_code_blocks_protected() {
        assert!(parse_markdown_formatting("```\n**not bold**\n```").is_none());
    }

    #[test]
    fn nested_bold_italic() {
        let r = parse_markdown_formatting("**_bold italic_**").unwrap();
        assert_eq!(r.clean_text, "bold italic");
        let all_styles: Vec<_> = r.formatting.iter().flat_map(|r| &r.styles).collect();
        assert!(all_styles.contains(&&TextFormattingStyle::Bold));
        assert!(all_styles.contains(&&TextFormattingStyle::Italic));
    }

    #[test]
    fn validate_valid_formatting() {
        let f = json!([{"start": 0, "length": 4, "styles": ["bold"]}]);
        assert!(validate_text_formatting(&f, "test").is_ok());
    }

    #[test]
    fn validate_empty_message() {
        let f = json!([{"start": 0, "length": 1, "styles": ["bold"]}]);
        assert!(validate_text_formatting(&f, "").is_err());
    }

    #[test]
    fn validate_range_exceeds() {
        let f = json!([{"start": 2, "length": 10, "styles": ["bold"]}]);
        assert!(validate_text_formatting(&f, "test").is_err());
    }

    #[test]
    fn validate_invalid_style() {
        let f = json!([{"start": 0, "length": 1, "styles": ["comic-sans"]}]);
        assert!(validate_text_formatting(&f, "test").is_err());
    }

    #[test]
    fn has_formatting_works() {
        assert!(!has_text_formatting(None));
        assert!(!has_text_formatting(Some(&json!([]))));
        assert!(has_text_formatting(Some(&json!([{"start": 0}]))));
    }
}
