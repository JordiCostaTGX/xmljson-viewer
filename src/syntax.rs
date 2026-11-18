use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Key,
    String,
    Number,
    Literal,
    Tag,
    Attribute,
    Comment,
    Punctuation,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Span {
    /// Character offsets within the line.
    pub range: Range<usize>,
    pub style: Style,
}

/// XML lexer state at a line boundary; JSON tokens never span lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Text,
    Tag,
    Value(char),
    Comment,
    CData,
    Markup,
}

pub fn json(chars: &[char]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        i += 1;
        let style = match chars[start] {
            '"' => {
                let mut escaped = false;
                while i < chars.len() {
                    let ch = chars[i];
                    i += 1;
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        break;
                    }
                }
                if chars[i..].iter().find(|c| !c.is_ascii_whitespace()) == Some(&':') {
                    Style::Key
                } else {
                    Style::String
                }
            }
            '-' | '0'..='9' => {
                while i < chars.len() && matches!(chars[i], '0'..='9' | '.' | 'e' | 'E' | '+' | '-')
                {
                    i += 1;
                }
                Style::Number
            }
            't' | 'f' | 'n' => {
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                Style::Literal
            }
            _ => continue,
        };
        spans.push(Span {
            range: start..i,
            style,
        });
    }
    spans
}

pub fn xml(chars: &[char], mut state: State) -> (Vec<Span>, State) {
    let n = chars.len();
    let starts = |at: usize, pattern: &str| {
        pattern
            .chars()
            .enumerate()
            .all(|(k, c)| chars.get(at + k) == Some(&c))
    };
    // Returns the offset just after `pattern`, if it occurs at or after `from`.
    let find = |from: usize, pattern: &str| {
        (from..n)
            .find(|&at| starts(at, pattern))
            .map(|at| at + pattern.chars().count())
    };
    let mut spans = Vec::new();
    let mut push = |range: Range<usize>, style| {
        if !range.is_empty() {
            spans.push(Span { range, style });
        }
    };
    let mut i = 0;
    while i < n {
        let start = i;
        match state {
            State::Comment | State::CData | State::Markup => {
                let (close, style) = match state {
                    State::Comment => ("-->", Style::Comment),
                    State::CData => ("]]>", Style::String),
                    _ => (">", Style::Literal),
                };
                match find(i, close) {
                    Some(end) => {
                        i = end;
                        state = State::Text;
                    }
                    None => i = n,
                }
                push(start..i, style);
            }
            State::Value(quote) => {
                match chars[i..].iter().position(|&c| c == quote) {
                    Some(at) => {
                        i += at + 1;
                        state = State::Tag;
                    }
                    None => i = n,
                }
                push(start..i, Style::String);
            }
            State::Tag => {
                let ch = chars[i];
                if ch == '>' || (ch == '/' && chars.get(i + 1) == Some(&'>')) {
                    i += if ch == '>' { 1 } else { 2 };
                    state = State::Text;
                    push(start..i, Style::Punctuation);
                } else if ch == '"' || ch == '\'' {
                    i += 1;
                    state = State::Value(ch);
                } else if ch == '=' || ch == '/' {
                    i += 1;
                    push(start..i, Style::Punctuation);
                } else if ch.is_whitespace() {
                    i += 1;
                } else {
                    while i < n
                        && !chars[i].is_whitespace()
                        && !matches!(chars[i], '=' | '>' | '/' | '"' | '\'')
                    {
                        i += 1;
                    }
                    push(start..i, Style::Attribute);
                }
                // An opening quote starts the value span, including the quote.
                if let State::Value(quote) = state {
                    match chars[i..].iter().position(|&c| c == quote) {
                        Some(at) => {
                            i += at + 1;
                            state = State::Tag;
                        }
                        None => i = n,
                    }
                    push(start..i, Style::String);
                }
            }
            State::Text => {
                if chars[i] == '<' {
                    let (open, close, next, style) = if starts(i, "<!--") {
                        ("<!--", "-->", State::Comment, Style::Comment)
                    } else if starts(i, "<![CDATA[") {
                        ("<![CDATA[", "]]>", State::CData, Style::String)
                    } else if starts(i, "<?") || starts(i, "<!") {
                        ("<!", ">", State::Markup, Style::Literal)
                    } else {
                        i += 1;
                        if chars.get(i) == Some(&'/') {
                            i += 1;
                        }
                        push(start..i, Style::Punctuation);
                        let name = i;
                        while i < n && !chars[i].is_whitespace() && !matches!(chars[i], '>' | '/') {
                            i += 1;
                        }
                        push(name..i, Style::Tag);
                        state = State::Tag;
                        continue;
                    };
                    match find(i + open.chars().count(), close) {
                        Some(end) => i = end,
                        None => {
                            i = n;
                            state = next;
                        }
                    }
                    push(start..i, style);
                } else if chars[i] == '&' {
                    match (i + 1..n.min(i + 12)).find(|&at| chars[at] == ';') {
                        Some(end) => {
                            i = end + 1;
                            push(start..i, Style::Literal);
                        }
                        None => i += 1,
                    }
                } else {
                    while i < n && !matches!(chars[i], '<' | '&') {
                        i += 1;
                    }
                }
            }
        }
    }
    (spans, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn unicode_escapes_and_incomplete_edits() {
        let text = r#"{"é":"😀\\","a":[-1.2e+3,true,false,null],"unfinished":"text"#;
        let spans = json(&chars(text));
        assert_eq!(spans[0].style, Style::Key);
        assert_eq!(spans[1].range, 5..10);
        assert_eq!(spans[1].style, Style::String);
        assert_eq!(spans[3].style, Style::Number);
        assert_eq!(spans.last().unwrap().range.end, text.chars().count());
    }

    #[test]
    fn colors_entire_long_string_without_length_cutoff() {
        let text = format!(r#"{{"data":"{}","next":true}}"#, "é😀".repeat(400_000));
        let spans = json(&chars(&text));
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[1].range.len(), 800_002);
    }

    #[test]
    fn xml_state_continues_across_lines() {
        let (spans, state) = xml(&chars(r#"<p:a x="1"#), State::Text);
        assert_eq!(
            spans.iter().map(|s| s.style).collect::<Vec<_>>(),
            [
                Style::Punctuation,
                Style::Tag,
                Style::Attribute,
                Style::Punctuation,
                Style::String
            ]
        );
        assert_eq!(state, State::Value('"'));
        let (spans, state) = xml(&chars(r#"2">t&amp;<!-- c"#), state);
        assert_eq!(spans[0].range, 0..2);
        assert_eq!(spans[0].style, Style::String);
        assert!(
            spans
                .iter()
                .any(|s| s.style == Style::Literal && s.range == (4..9))
        );
        assert_eq!(state, State::Comment);
        let (spans, state) = xml(&chars("--><![CDATA[x]]></p:a>"), state);
        assert_eq!(spans[0].style, Style::Comment);
        assert_eq!(spans[1].style, Style::String);
        assert_eq!(state, State::Text);
    }

    #[test]
    fn xml_declarations_and_self_closing_tags() {
        let (spans, state) = xml(&chars(r#"<?xml version="1.0"?><b/>"#), State::Text);
        assert_eq!(spans[0].style, Style::Literal);
        assert_eq!(spans[0].range, 0..21);
        assert_eq!(spans.last().unwrap().style, Style::Punctuation);
        assert_eq!(state, State::Text);
    }
}
