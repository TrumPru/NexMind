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
    fn test_split_message_at_newline() {
        let text = format!("{}\n{}", "a".repeat(3000), "b".repeat(2000));
        let parts = split_message(&text, 4096);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 3000);
    }
}
