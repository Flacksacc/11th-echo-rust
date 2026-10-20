//! Incremental framing for one model response with independent speech and data.
use serde::Deserialize;

pub const INSTRUCTIONS: &str = r#"Return your final answer in this exact order, with each delimiter on its own line:
<echo_assistant>
Conversational text for speech.
</echo_assistant>
<echo_data>
The complete written answer, including useful details and source links.
</echo_data>
<echo_files>
[]
</echo_files>
For a plain answer of at most 120 words and one paragraph, repeat that answer in both text sections. For longer or structured answers, the spoken section must be a simple summary of at most 60 words followed by a short invitation to read the information below. Never read pages of documentation aloud. The files section is a JSON array: either {"artifact_id":"an ID returned by a tool"} or {"name":"document.md","content":"UTF-8 text","media_type":"text/markdown"}. Do not invent paths or artifact IDs. Escape a literal delimiter line by prefixing it with a backslash. Use tools BEFORE this final response; never request tools after starting it. Do not emit these tags in tool-planning messages."#;

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum FileSpec {
    Existing {
        artifact_id: String,
    },
    Generated {
        name: String,
        content: String,
        #[serde(default)]
        media_type: String,
    },
}

#[derive(Default, Clone, Debug)]
pub struct Channels {
    pub assistant: String,
    pub data: String,
    pub files: Vec<FileSpec>,
}

#[derive(Default)]
pub struct Parser {
    raw: String,
    channels: Channels,
}

impl Parser {
    pub fn update(&mut self, cumulative: &str) -> Result<&Channels, String> {
        if !cumulative.starts_with(&self.raw) {
            return Err("Response stream changed previously delivered text.".into());
        }
        if cumulative.len() > 1024 * 1024 {
            return Err("Response exceeded the size limit.".into());
        }
        self.raw = cumulative.into();
        self.channels = Self::parse(cumulative, false)?;
        Ok(&self.channels)
    }
    pub fn finish(&mut self) -> Result<Channels, String> {
        self.channels = Self::parse(&self.raw, true)?;
        Ok(self.channels.clone())
    }
    fn parse(raw: &str, final_chunk: bool) -> Result<Channels, String> {
        let tags = [
            "<echo_assistant>",
            "</echo_assistant>",
            "<echo_data>",
            "</echo_data>",
            "<echo_files>",
            "</echo_files>",
        ];
        let mut stage = 0;
        let mut sections = [String::new(), String::new(), String::new()];
        for line in raw.split_inclusive('\n') {
            let complete = line.ends_with('\n') || final_chunk;
            let clean = line.trim_end_matches(['\r', '\n']);
            if let Some(index) = tags.iter().position(|tag| *tag == clean) {
                if !complete {
                    break;
                }
                if index != stage {
                    return Err("Response sections are duplicated or out of order.".into());
                }
                stage += 1;
            } else if !complete && tags.iter().any(|tag| tag.starts_with(clean)) {
                break;
            } else if stage == 1 || stage == 3 || stage == 5 {
                let value = clean
                    .strip_prefix('\\')
                    .filter(|s| tags.contains(s))
                    .unwrap_or(clean);
                sections[(stage - 1) / 2].push_str(value);
                if line.ends_with('\n') {
                    sections[(stage - 1) / 2].push('\n');
                }
            } else if !clean.trim().is_empty() {
                return Err("Model did not follow the assistant response format.".into());
            }
        }
        if sections[0].split_whitespace().count() > 120 {
            return Err("Spoken response exceeded 120 words.".into());
        }
        if final_chunk && stage != 6 {
            return Err("Model returned an incomplete assistant response.".into());
        }
        let files = if stage == 6 {
            serde_json::from_str(sections[2].trim())
                .map_err(|_| "Invalid response file manifest.")?
        } else {
            vec![]
        };
        let data = sections[1].trim_end_matches(['\r', '\n']).to_owned();
        let assistant = sections[0].trim_end_matches(['\r', '\n']).to_owned();
        if final_chunk && (assistant.trim().is_empty() || data.trim().is_empty()) {
            return Err("Assistant and data responses must contain text.".into());
        }
        Ok(Channels {
            assistant,
            data,
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ANSWER: &str = "<echo_assistant>\nHello!\n</echo_assistant>\n<echo_data>\nFull information.\n</echo_data>\n<echo_files>\n[]\n</echo_files>\n";
    #[test]
    fn every_split_streams_without_delimiters() {
        for split in 0..=ANSWER.len() {
            let mut parser = Parser::default();
            let partial = parser.update(&ANSWER[..split]).unwrap();
            assert!(!partial.assistant.contains("<echo_"));
            assert!(!partial.data.contains("<echo_"));
            parser.update(ANSWER).unwrap();
            let result = parser.finish().unwrap();
            assert_eq!(result.assistant, "Hello!");
            assert_eq!(result.data, "Full information.");
        }
    }
    #[test]
    fn bad_order_and_incomplete_are_errors() {
        assert!(Parser::default().update("<echo_data>\n").is_err());
        let mut parser = Parser::default();
        parser.update("<echo_assistant>\nPartial").unwrap();
        assert!(parser.finish().is_err());
    }
    #[test]
    fn escaped_tags_and_unicode_are_content() {
        let mut parser = Parser::default();
        let raw = ANSWER.replace("Full information.", "Résumé\n\\</echo_data>");
        for (index, _) in raw.char_indices() {
            parser.update(&raw[..index]).unwrap();
        }
        parser.update(&raw).unwrap();
        assert_eq!(parser.finish().unwrap().data, "Résumé\n</echo_data>");
    }
}
