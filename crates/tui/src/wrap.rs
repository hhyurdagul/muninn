use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Word-wraps `text` to `width` display columns. Newlines are kept, words
/// longer than a line are split, and an empty input yields one empty line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_inclusive(' ') {
            if line.width() + word.trim_end().width() > width && !line.is_empty() {
                out.push(line.trim_end().to_string());
                line.clear();
            }
            for ch in word.chars() {
                if line.width() + ch.width().unwrap_or(0) > width && ch != ' ' {
                    out.push(std::mem::take(&mut line));
                }
                line.push(ch);
            }
        }
        out.push(line.trim_end().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wraps_words_breaks_long_ones_and_keeps_blank_lines() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"]);
        assert_eq!(wrap("", 10), [""]);
        assert_eq!(wrap("日本語テキスト", 6), ["日本語", "テキス", "ト"]);
    }
}
