//! Telegram-specific message formatting utilities.

/// Escape special characters for Telegram MarkdownV2.
pub fn escape_markdown_v2(text: &str) -> String {
    let special_chars = [
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if special_chars.contains(&ch) {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

/// Escape special characters for Telegram HTML formatting.
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Split a long message into chunks that fit Telegram's 4096 char limit.
pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut parts = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            parts.push(remaining.to_string());
            break;
        }

        // Try to split at a newline near the limit
        let chunk = &remaining[..max_len];
        let split_at = chunk.rfind('\n').unwrap_or(max_len);
        let split_at = if split_at == 0 { max_len } else { split_at };

        parts.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start_matches('\n');
    }

    parts
}

/// Convert basic Markdown to Telegram HTML.
///
/// Handles common patterns LLMs produce:
/// - **bold** → <b>bold</b>
/// - *italic* → <i>italic</i>
/// - `code` → <code>code</code>
/// - ```block``` → <pre>block</pre>
/// - [text](url) → <a href="url">text</a>
///
/// Any remaining HTML-special characters are escaped.
pub fn markdown_to_telegram_html(text: &str) -> String {
    let mut result = String::with_capacity(text.len());

    // First pass: handle code blocks (``` ... ```)
    let mut segments: Vec<(bool, String)> = Vec::new(); // (is_code_block, content)
    let text_str = text;
    let len = text_str.len();
    #[allow(unused_assignments)]
    let mut pos = 0;

    loop {
        if let Some(start) = text_str[pos..].find("```") {
            let abs_start = pos + start;
            // Push text before code block
            if abs_start > pos {
                segments.push((false, text_str[pos..abs_start].to_string()));
            }
            // Find closing ```
            let code_start = abs_start + 3;
            // Skip optional language tag on first line
            let code_content_start = if let Some(nl) = text_str[code_start..].find('\n') {
                let first_line = &text_str[code_start..code_start + nl];
                // If first line looks like a language tag (no spaces, short), skip it
                if !first_line.is_empty() && !first_line.contains(' ') && first_line.len() < 20 {
                    code_start + nl + 1
                } else {
                    code_start
                }
            } else {
                code_start
            };

            if let Some(end) = text_str[code_start..].find("```") {
                let abs_end = code_start + end;
                let code = &text_str[code_content_start..abs_end];
                segments.push((true, code.to_string()));
                pos = abs_end + 3;
            } else {
                // No closing ```, treat as text
                segments.push((false, text_str[abs_start..].to_string()));
                pos = len;
                break;
            }
        } else {
            if pos < text_str.len() {
                segments.push((false, text_str[pos..].to_string()));
            }
            break;
        }
    }

    // Now process each segment
    for (is_code, content) in segments {
        if is_code {
            result.push_str("<pre>");
            result.push_str(&escape_html(content.trim()));
            result.push_str("</pre>");
        } else {
            result.push_str(&convert_inline_markdown(&content));
        }
    }

    result
}

/// Convert inline markdown (bold, italic, code, links) to HTML.
fn convert_inline_markdown(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Inline code: `code`
        if chars[i] == '`' {
            if let Some(end) = find_closing(&chars, i + 1, '`') {
                let code: String = chars[i + 1..end].iter().collect();
                result.push_str("<code>");
                result.push_str(&escape_html(&code));
                result.push_str("</code>");
                i = end + 1;
                continue;
            }
        }

        // Links: [text](url)
        if chars[i] == '[' {
            if let Some(bracket_end) = find_closing(&chars, i + 1, ']') {
                if bracket_end + 1 < len && chars[bracket_end + 1] == '(' {
                    if let Some(paren_end) = find_closing(&chars, bracket_end + 2, ')') {
                        let link_text: String = chars[i + 1..bracket_end].iter().collect();
                        let url: String = chars[bracket_end + 2..paren_end].iter().collect();
                        result.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            escape_html(&url),
                            escape_html(&link_text)
                        ));
                        i = paren_end + 1;
                        continue;
                    }
                }
            }
        }

        // Bold: **text**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_double_closing(&chars, i + 2, '*') {
                let bold: String = chars[i + 2..end].iter().collect();
                result.push_str("<b>");
                result.push_str(&escape_html(&bold));
                result.push_str("</b>");
                i = end + 2;
                continue;
            }
        }

        // Italic: *text* (single asterisk, not double)
        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            if let Some(end) = find_single_closing(&chars, i + 1, '*') {
                let italic: String = chars[i + 1..end].iter().collect();
                result.push_str("<i>");
                result.push_str(&escape_html(&italic));
                result.push_str("</i>");
                i = end + 1;
                continue;
            }
        }

        // Escape HTML special chars for regular text
        match chars[i] {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            ch => result.push(ch),
        }
        i += 1;
    }

    result
}

fn find_closing(chars: &[char], start: usize, close: char) -> Option<usize> {
    for j in start..chars.len() {
        if chars[j] == close {
            return Some(j);
        }
        if chars[j] == '\n' && close != '\n' {
            return None; // Don't cross line boundaries for inline
        }
    }
    None
}

fn find_double_closing(chars: &[char], start: usize, ch: char) -> Option<usize> {
    let mut j = start;
    while j + 1 < chars.len() {
        if chars[j] == ch && chars[j + 1] == ch {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn find_single_closing(chars: &[char], start: usize, ch: char) -> Option<usize> {
    for j in start..chars.len() {
        if chars[j] == ch {
            // Make sure it's not a double (bold)
            if j + 1 < chars.len() && chars[j + 1] == ch {
                return None;
            }
            return Some(j);
        }
        if chars[j] == '\n' {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_markdown_v2() {
        assert_eq!(escape_markdown_v2("hello_world"), "hello\\_world");
        assert_eq!(escape_markdown_v2("**bold**"), "\\*\\*bold\\*\\*");
        assert_eq!(escape_markdown_v2("normal text"), "normal text");
        assert_eq!(escape_markdown_v2("a.b.c"), "a\\.b\\.c");
    }

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("<b>bold</b>"), "&lt;b&gt;bold&lt;/b&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("normal"), "normal");
    }

    #[test]
    fn test_split_message_short() {
        let parts = split_message("Hello!", 4096);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], "Hello!");
    }

    #[test]
    fn test_split_message_long() {
        let long_text = "a".repeat(5000);
        let parts = split_message(&long_text, 4096);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 4096);
        assert_eq!(parts[1].len(), 904);
    }

    #[test]
    fn test_markdown_to_html_bold() {
        assert_eq!(
            markdown_to_telegram_html("Hello **world**!"),
            "Hello <b>world</b>!"
        );
    }

    #[test]
    fn test_markdown_to_html_italic() {
        assert_eq!(
            markdown_to_telegram_html("Hello *world*!"),
            "Hello <i>world</i>!"
        );
    }

    #[test]
    fn test_markdown_to_html_code() {
        assert_eq!(
            markdown_to_telegram_html("Use `git commit` here"),
            "Use <code>git commit</code> here"
        );
    }

    #[test]
    fn test_markdown_to_html_code_block() {
        assert_eq!(
            markdown_to_telegram_html("```\nsome code\n```"),
            "<pre>some code</pre>"
        );
    }

    #[test]
    fn test_markdown_to_html_code_block_with_lang() {
        assert_eq!(
            markdown_to_telegram_html("```python\nprint('hi')\n```"),
            "<pre>print('hi')</pre>"
        );
    }

    #[test]
    fn test_markdown_to_html_link() {
        assert_eq!(
            markdown_to_telegram_html("See [Google](https://google.com)"),
            "See <a href=\"https://google.com\">Google</a>"
        );
    }

    #[test]
    fn test_markdown_to_html_escapes_html() {
        assert_eq!(
            markdown_to_telegram_html("a < b & c > d"),
            "a &lt; b &amp; c &gt; d"
        );
    }

    #[test]
    fn test_markdown_to_html_mixed() {
        let input = "**NexMind** — ваш `AI` помощник";
        let output = markdown_to_telegram_html(input);
        assert!(output.contains("<b>NexMind</b>"));
        assert!(output.contains("<code>AI</code>"));
    }

    #[test]
    fn test_markdown_to_html_plain_text() {
        assert_eq!(
            markdown_to_telegram_html("Just plain text"),
            "Just plain text"
        );
    }

    #[test]
    fn test_split_message_at_newline() {
        let text = format!("{}\n{}", "a".repeat(3000), "b".repeat(2000));
        let parts = split_message(&text, 4096);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 3000);
    }
}
