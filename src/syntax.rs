use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Key,
    String,
    Number,
    Literal,
}

#[derive(Debug)]
pub struct Span {
    pub range: Range<i32>,
    pub style: Style,
}

pub struct Highlight {
    pub spans: Vec<Span>,
    pub long_lines: Vec<Range<i32>>,
}

pub fn analyze(text: &str, cancelled: impl Fn() -> bool) -> Highlight {
    let spans = json(text, &cancelled);
    let mut long_lines = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        if cancelled() {
            break;
        }
        let end = start + line.chars().count() as i32;
        if end - start > 2048 {
            long_lines.push(start..end);
        }
        start = end + 1;
    }
    Highlight { spans, long_lines }
}

pub fn json(text: &str, cancelled: impl Fn() -> bool) -> Vec<Span> {
    let mut result = Vec::new();
    let mut chars = text.chars().peekable();
    let mut offset = 0;
    while let Some(ch) = chars.next() {
        if cancelled() {
            break;
        }
        let start = offset;
        offset += 1;
        let style = match ch {
            '"' => {
                let mut escaped = false;
                for ch in chars.by_ref() {
                    offset += 1;
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        break;
                    }
                }
                let mut lookahead = chars.clone();
                if lookahead.find(|c| !c.is_ascii_whitespace()) == Some(':') {
                    Style::Key
                } else {
                    Style::String
                }
            }
            '-' | '0'..='9' => {
                while chars
                    .peek()
                    .is_some_and(|c| matches!(c, '0'..='9' | '.' | 'e' | 'E' | '+' | '-'))
                {
                    chars.next();
                    offset += 1;
                }
                Style::Number
            }
            't' | 'f' | 'n' => {
                while chars.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                    chars.next();
                    offset += 1;
                }
                Style::Literal
            }
            _ => continue,
        };
        result.push(Span {
            range: start..offset,
            style,
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_escapes_and_incomplete_edits() {
        let text = r#"{"é":"😀\\","a":[-1.2e+3,true,false,null],"unfinished":"text"#;
        let spans = json(text, || false);
        assert_eq!(spans[0].style, Style::Key);
        assert_eq!(spans[1].range, 5..10);
        assert_eq!(spans[1].style, Style::String);
        assert_eq!(spans[3].style, Style::Number);
        assert_eq!(spans.last().unwrap().range.end, text.chars().count() as i32);
    }

    #[test]
    fn colors_entire_long_string_without_length_cutoff() {
        let text = format!(r#"{{"data":"{}","next":true}}"#, "é😀".repeat(400_000));
        let spans = json(&text, || false);
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[1].range.end - spans[1].range.start, 800_002);
        assert!(json(&text, || true).is_empty());
    }
}
