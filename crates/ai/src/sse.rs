/// Incremental Server-Sent Events parser that yields `data:` payloads.
#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    /// Feed raw bytes; returns every complete `data:` payload seen so far.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        let mut payloads = Vec::new();
        while let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(data) = line.strip_prefix("data:") {
                payloads.push(data.trim_start().to_string());
            }
        }
        payloads
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_payloads_across_chunks_and_ignores_comments() {
        let mut parser = SseParser::default();
        assert!(parser.push(b": keep-alive\n\ndata: {\"a\"").is_empty());
        assert_eq!(parser.push(b":1}\r\n\ndata: [DONE]\n"), vec!["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn keeps_multibyte_characters_split_between_chunks() {
        let mut parser = SseParser::default();
        let bytes = "data: ğ\n".as_bytes();
        assert!(parser.push(&bytes[..7]).is_empty());
        assert_eq!(parser.push(&bytes[7..]), vec!["ğ"]);
    }
}
