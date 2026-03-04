#[derive(Debug, Default, Clone)]
pub struct TranscriptPipeline {
    transcript: String,
    stop_requested: bool,
}

impl TranscriptPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_fragment(&mut self, fragment: &str) -> String {
        self.transcript = append_fragment(&self.transcript, fragment);
        self.transcript.clone()
    }

    pub fn request_stop(&mut self) {
        self.stop_requested = true;
    }

    pub fn committed_text(&self) -> &str {
        &self.transcript
    }
}

pub fn append_fragment(existing: &str, incoming: &str) -> String {
    let incoming = incoming.trim();
    if incoming.is_empty() {
        return existing.to_string();
    }

    let mut output = existing.trim().to_string();
    if output.is_empty() {
        return incoming.to_string();
    }

    let no_leading_space = [".", ",", "!", "?", ";", ":"];
    if no_leading_space.iter().any(|p| incoming.starts_with(p)) {
        output.push_str(incoming);
    } else {
        output.push(' ');
        output.push_str(incoming);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{append_fragment, TranscriptPipeline};

    #[test]
    fn append_fragment_adds_spaces_between_words() {
        let a = append_fragment("hello", "world");
        assert_eq!(a, "hello world");
    }

    #[test]
    fn append_fragment_avoids_space_before_punctuation() {
        let a = append_fragment("hello world", "!");
        assert_eq!(a, "hello world!");
    }

    #[test]
    fn pipeline_requires_stop_before_injection_payload() {
        let mut p = TranscriptPipeline::new();
        p.push_fragment("hello");
        p.push_fragment("world");
        assert_eq!(p.committed_text(), "hello world");
    }

    #[test]
    fn pipeline_returns_full_text_after_stop() {
        let mut p = TranscriptPipeline::new();
        p.push_fragment("hello");
        p.push_fragment("world");
        p.request_stop();
        assert_eq!(p.committed_text(), "hello world");
    }

    #[test]
    fn empty_after_new_has_no_payload() {
        let p = TranscriptPipeline::new();
        assert_eq!(p.committed_text(), "");
    }

    #[test]
    fn committed_text_reflects_added_fragments() {
        let mut p = TranscriptPipeline::new();
        p.push_fragment("alpha");
        p.push_fragment("beta");
        assert_eq!(p.committed_text(), "alpha beta");
    }

    #[test]
    fn append_ignores_empty_fragment() {
        let a = append_fragment("hello", "   ");
        assert_eq!(a, "hello");
    }
}
