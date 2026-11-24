use quick_xml::{
    Reader, Writer,
    events::{BytesText, Event},
};
use serde::Deserialize;
use std::{ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Xml,
    Json,
}

impl Kind {
    pub fn detect(text: &str) -> Self {
        if text
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with('<')
        {
            Self::Xml
        } else {
            Self::Json
        }
    }
}

#[derive(Debug)]
pub enum ValueStyle {
    Container,
    String,
    Number,
    Literal,
}

#[derive(Debug)]
pub struct AttributeStyle {
    pub name: Range<u32>,
    pub value: Range<u32>,
}

#[derive(Debug, Default)]
pub enum LabelStyle {
    #[default]
    Plain,
    Json {
        key_end: Option<u32>,
        value: ValueStyle,
    },
    Xml {
        element_end: u32,
        attributes: Box<[AttributeStyle]>,
    },
    Text,
}

#[derive(Debug)]
pub struct Node {
    pub label: String,
    pub label_style: LabelStyle,
    pub range: Range<usize>,
    pub chars: Range<i32>,
    pub parent: Option<usize>,
    pub children: Arc<[usize]>,
    pub structural: bool,
}

#[derive(Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub roots: Arc<[usize]>,
}

impl Document {
    pub fn at_cursor(&self, position: i32) -> Option<usize> {
        // Preorder means the last containing node is the innermost element.
        self.nodes.iter().enumerate().rev().find_map(|(id, node)| {
            (node.structural && node.chars.start <= position && position < node.chars.end)
                .then_some(id)
        })
    }
}

fn preview(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut value: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        value.push('…');
    }
    value
}

struct Builder {
    nodes: Vec<Node>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

impl Builder {
    fn new() -> Self {
        Self {
            nodes: vec![],
            children: vec![],
            roots: vec![],
        }
    }

    fn add(
        &mut self,
        label: String,
        range: Range<usize>,
        parent: Option<usize>,
    ) -> Result<usize, String> {
        if self.nodes.len() >= 2_000_000 {
            return Err("The document exceeds the safety limit of 2 million nodes".into());
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            label,
            label_style: LabelStyle::Plain,
            range,
            chars: 0..0,
            parent,
            children: Arc::from([]),
            structural: true,
        });
        self.children.push(vec![]);
        match parent {
            Some(parent) => self.children[parent].push(id),
            None => self.roots.push(id),
        }
        Ok(id)
    }

    fn finish(mut self, text: &str) -> Result<Document, String> {
        // One linear UTF-8 pass maps all byte ranges to editor character offsets.
        let mut endpoints: Vec<(usize, usize, bool)> = self
            .nodes
            .iter()
            .enumerate()
            .flat_map(|(id, n)| [(n.range.start, id, false), (n.range.end, id, true)])
            .collect();
        endpoints.sort_unstable();
        let mut byte = 0;
        let mut character: i32 = 0;
        for (offset, id, end) in endpoints {
            character = character
                .checked_add(
                    i32::try_from(text[byte..offset].chars().count())
                        .map_err(|_| "Document too large for the editor")?,
                )
                .ok_or("Document too large for the editor")?;
            byte = offset;
            if end {
                self.nodes[id].chars.end = character;
            } else {
                self.nodes[id].chars.start = character;
            }
        }
        for (node, children) in self.nodes.iter_mut().zip(self.children) {
            node.children = children.into();
        }
        Ok(Document {
            nodes: self.nodes,
            roots: self.roots.into(),
        })
    }
}

pub fn parse(text: Arc<str>, kind: Kind, cancelled: impl Fn() -> bool) -> Result<Document, String> {
    if text.len() > i32::MAX as usize {
        return Err("Document too large for the editor".into());
    }
    match kind {
        Kind::Json => parse_json(text, &cancelled),
        Kind::Xml => parse_xml(text, &cancelled),
    }
}

fn parse_json(text: Arc<str>, cancelled: &impl Fn() -> bool) -> Result<Document, String> {
    // Windows tools often save UTF-8 with a byte order mark.
    let start = text.len() - text.trim_start_matches('\u{feff}').len();
    let mut deserializer = serde_json::Deserializer::from_str(&text[start..]);
    serde::de::IgnoredAny::deserialize(&mut deserializer).map_err(|e| format!("JSON: {e}"))?;
    deserializer.end().map_err(|e| format!("JSON: {e}"))?;
    let mut parser = JsonScanner {
        text: &text,
        offset: start,
        builder: Builder::new(),
        cancelled,
    };
    parser.value("root", None, None)?;
    parser.builder.finish(&text)
}

struct JsonScanner<'a, F> {
    text: &'a str,
    offset: usize,
    builder: Builder,
    cancelled: &'a F,
}

impl<F: Fn() -> bool> JsonScanner<'_, F> {
    fn whitespace(&mut self) {
        while self.offset < self.text.len()
            && self.text.as_bytes()[self.offset].is_ascii_whitespace()
        {
            self.offset += 1;
        }
    }

    fn string_end(&mut self) {
        self.offset += 1;
        while self.offset < self.text.len() {
            match self.text.as_bytes()[self.offset] {
                b'\\' => self.offset += 2,
                b'"' => {
                    self.offset += 1;
                    break;
                }
                _ => self.offset += 1,
            }
        }
    }

    fn value(
        &mut self,
        key: &str,
        parent: Option<usize>,
        key_start: Option<usize>,
    ) -> Result<usize, String> {
        if (self.cancelled)() {
            return Err("Parsing cancelled by a newer edit".into());
        }
        self.whitespace();
        let start = key_start.unwrap_or(self.offset);
        let value_start = self.offset;
        let token = self.text.as_bytes()[self.offset];
        let id = self.builder.add(String::new(), start..start, parent)?;
        let label = if token == b'{' || token == b'[' {
            self.offset += 1;
            let closing = if token == b'{' { b'}' } else { b']' };
            self.whitespace();
            let mut count = 0;
            while self.text.as_bytes()[self.offset] != closing {
                if token == b'{' {
                    let key_position = self.offset;
                    self.string_end();
                    let child_key: String =
                        serde_json::from_str(&self.text[key_position..self.offset])
                            .map_err(|e| format!("JSON: {e}"))?;
                    self.whitespace();
                    self.offset += 1;
                    self.value(&child_key, Some(id), Some(key_position))?;
                } else {
                    self.value(&format!("[{count}]"), Some(id), None)?;
                }
                count += 1;
                self.whitespace();
                if self.text.as_bytes()[self.offset] == b',' {
                    self.offset += 1;
                    self.whitespace();
                }
            }
            self.offset += 1;
            if token == b'{' {
                format!("{{{count}}}")
            } else {
                format!("[{count}]")
            }
        } else {
            if token == b'"' {
                self.string_end();
            } else {
                while self.offset < self.text.len()
                    && !matches!(
                        self.text.as_bytes()[self.offset],
                        b',' | b'}' | b']' | b' ' | b'\n' | b'\t' | b'\r'
                    )
                {
                    self.offset += 1;
                }
            }
            preview(&self.text[value_start..self.offset], 80)
        };
        let key = if parent.is_some() {
            preview(key, 80)
        } else {
            String::new()
        };
        self.builder.nodes[id].label_style = LabelStyle::Json {
            key_end: parent.map(|_| key.len() as u32),
            value: match token {
                b'{' | b'[' => ValueStyle::Container,
                b'"' => ValueStyle::String,
                b't' | b'f' | b'n' => ValueStyle::Literal,
                _ => ValueStyle::Number,
            },
        };
        self.builder.nodes[id].label = if parent.is_none() {
            label
        } else {
            format!("{key}: {label}")
        };
        self.builder.nodes[id].range.end = self.offset;
        Ok(id)
    }
}

fn parse_xml(text: Arc<str>, cancelled: &impl Fn() -> bool) -> Result<Document, String> {
    let source = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let base = text.len() - source.len();
    let mut reader = Reader::from_str(source);
    let mut builder = Builder::new();
    let mut stack = Vec::new();
    loop {
        if cancelled() {
            return Err("Parsing cancelled by a newer edit".into());
        }
        let start = reader.buffer_position() as usize + base;
        let event = reader
            .read_event()
            .map_err(|e| format!("XML at byte {start}: {e}"))?;
        let end = reader.buffer_position() as usize + base;
        match event {
            Event::Start(ref el) | Event::Empty(ref el) => {
                if stack.len() >= 512 {
                    return Err("XML: depth exceeds the safety limit of 512".into());
                }
                let name =
                    std::str::from_utf8(el.name().into_inner()).map_err(|e| e.to_string())?;
                let mut label = format!("<{}", name);
                let element_end = label.len() as u32;
                let mut attributes = Vec::new();
                for attr in el.attributes() {
                    let attr = attr.map_err(|e| format!("XML: {e}"))?;
                    let key =
                        std::str::from_utf8(attr.key.into_inner()).map_err(|e| e.to_string())?;
                    let value = attr.unescape_value().map_err(|e| format!("XML: {e}"))?;
                    if label.len() < 120 {
                        label.push(' ');
                        let name_start = label.len() as u32;
                        label.push_str(key);
                        let name_end = label.len() as u32;
                        label.push_str("=\"");
                        let value_start = label.len() as u32;
                        label.push_str(&preview(&value, 50));
                        let value_end = label.len() as u32;
                        label.push('"');
                        attributes.push(AttributeStyle {
                            name: name_start..name_end,
                            value: value_start..value_end,
                        });
                    }
                }
                label.push('>');
                let id = builder.add(label, start..end, stack.last().copied())?;
                builder.nodes[id].label_style = LabelStyle::Xml {
                    element_end,
                    attributes: attributes.into_boxed_slice(),
                };
                if matches!(event, Event::Start(_)) {
                    stack.push(id);
                }
            }
            Event::End(_) => {
                let id = stack.pop().ok_or("XML: closing tag without opening tag")?;
                builder.nodes[id].range.end = end;
            }
            Event::Text(ref data) => {
                let value = data.unescape().map_err(|e| format!("XML: {e}"))?;
                if !value.trim().is_empty() && value.trim() != "\u{feff}" {
                    let parent = stack
                        .last()
                        .copied()
                        .ok_or("XML: text outside an element")?;
                    let id = builder.add(preview(value.trim(), 80), start..end, Some(parent))?;
                    builder.nodes[id].structural = false;
                    builder.nodes[id].label_style = LabelStyle::Text;
                }
            }
            Event::CData(ref data) => {
                let parent = stack
                    .last()
                    .copied()
                    .ok_or("XML: CDATA outside an element")?;
                let value = std::str::from_utf8(data).map_err(|e| e.to_string())?;
                let id = builder.add(
                    format!("CDATA: {}", preview(value, 80)),
                    start..end,
                    Some(parent),
                )?;
                builder.nodes[id].structural = false;
                builder.nodes[id].label_style = LabelStyle::Text;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("XML: unclosed element".into());
    }
    if builder.roots.is_empty() {
        return Err("XML: no elements found".into());
    }
    builder.finish(&text)
}

pub fn format(text: &str, kind: Kind) -> Result<String, String> {
    match kind {
        Kind::Json => {
            // Formatting tokens rather than Value preserves duplicate keys and number precision.
            parse(Arc::from(text), kind, || false)?;
            let mut result = String::with_capacity(text.len());
            let mut depth = 0;
            let mut in_string = false;
            let mut escaped = false;
            let mut previous = '\0';
            for ch in text.chars() {
                if in_string {
                    result.push(ch);
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_string = false;
                    }
                    continue;
                }
                if ch.is_whitespace() {
                    continue;
                }
                if (previous == '{' || previous == '[') && ch != '}' && ch != ']' {
                    result.push('\n');
                    result.push_str(&"  ".repeat(depth));
                }
                match ch {
                    '"' => {
                        result.push(ch);
                        in_string = true;
                    }
                    '{' | '[' => {
                        result.push(ch);
                        depth += 1;
                    }
                    '}' | ']' => {
                        depth -= 1;
                        if previous != '{' && previous != '[' {
                            result.push('\n');
                            result.push_str(&"  ".repeat(depth));
                        }
                        result.push(ch);
                    }
                    ',' => {
                        result.push_str(",\n");
                        result.push_str(&"  ".repeat(depth));
                    }
                    ':' => result.push_str(": "),
                    _ => result.push(ch),
                }
                previous = ch;
            }
            Ok(result)
        }
        Kind::Xml => {
            let document = parse(Arc::from(text), kind, || false)?;
            // Adding whitespace to mixed content changes its value. Preserve such documents.
            if document.nodes.iter().any(|node| {
                let mut elements = false;
                let mut text = false;
                for &id in node.children.iter() {
                    if document.nodes[id].structural {
                        elements = true;
                    } else {
                        text = true;
                    }
                }
                elements && text
            }) {
                return Ok(text.into());
            }
            let mut reader = Reader::from_str(text);
            let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
            loop {
                let event = reader.read_event().map_err(|e| format!("XML: {e}"))?;
                match event {
                    Event::Eof => break,
                    Event::Text(ref t)
                        if t.unescape().map_err(|e| e.to_string())?.trim().is_empty() => {}
                    other => writer.write_event(other).map_err(|e| e.to_string())?,
                }
            }
            String::from_utf8(writer.into_inner()).map_err(|e| e.to_string())
        }
    }
}

fn embedded_xml(text: &str) -> Option<(&str, &str, &str)> {
    let start = if text.trim_start().starts_with("postData:[") {
        text.find("<?xml").or_else(|| text.find('<'))?
    } else {
        let trimmed_start = text.len() - text.trim_start().len();
        let trimmed = &text[trimmed_start..];
        (trimmed.starts_with("<?xml") || trimmed.starts_with('<')).then_some(trimmed_start)?
    };
    let end = text.rfind('>')?.checked_add(1)?;
    (start < end).then_some((&text[..start], &text[start..end], &text[end..]))
}

fn write_decoded_xml(
    writer: &mut Writer<Vec<u8>>,
    raw: &str,
    depth: usize,
) -> Result<bool, String> {
    if depth > 8 {
        return Err("XML nesting exceeds the decode limit".into());
    }
    let mut reader = Reader::from_str(raw);
    let mut changed = false;
    loop {
        let event = reader.read_event().map_err(|e| format!("XML: {e}"))?;
        match event {
            Event::Eof => break,
            Event::Text(text) => {
                let decoded = text.unescape().map_err(|e| e.to_string())?;
                if let Some((prefix, payload, suffix)) = embedded_xml(&decoded)
                    && let Ok(formatted) = format(payload, Kind::Xml)
                {
                    if !prefix.is_empty() {
                        writer
                            .write_event(Event::Text(BytesText::new(prefix)))
                            .map_err(|e| e.to_string())?;
                    }
                    write_decoded_xml(writer, &formatted, depth + 1)?;
                    if !suffix.is_empty() {
                        writer
                            .write_event(Event::Text(BytesText::new(suffix)))
                            .map_err(|e| e.to_string())?;
                    }
                    changed = true;
                    continue;
                }
                writer
                    .write_event(Event::Text(text))
                    .map_err(|e| e.to_string())?;
            }
            Event::Decl(_) if depth > 0 => {}
            other => writer.write_event(other).map_err(|e| e.to_string())?,
        }
    }
    Ok(changed)
}

fn decode_embedded_xml(raw: &str) -> Result<Option<String>, String> {
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
    if !write_decoded_xml(&mut writer, raw, 0)? {
        return Ok(None);
    }
    String::from_utf8(writer.into_inner())
        .map(Some)
        .map_err(|e| e.to_string())
}

fn decode_json_properties(raw: &str) -> Result<Option<String>, String> {
    let json_wrapper = regex::Regex::new(
        r#"(?is)(?P<prefix>"(?:\\.|[^"\\])*"\s*:\s*)(?P<value>"(?:\\.|[^"\\])*")"#,
    )
    .map_err(|e| e.to_string())?;
    let mut output = String::with_capacity(raw.len());
    let mut copied_until = 0;
    let mut changed = false;
    for parts in json_wrapper.captures_iter(raw) {
        let prefix = parts.name("prefix").ok_or("JSON property has no key")?;
        let inner = parts.name("value").ok_or("JSON property has no value")?;
        let Some((decoded, kind)) = decode_content(inner.as_str())? else {
            continue;
        };
        let value = if kind == Some(Kind::Json) {
            decoded
        } else {
            serde_json::to_string(&decoded).map_err(|e| e.to_string())?
        };
        if value == inner.as_str() {
            continue;
        }
        output.push_str(&raw[copied_until..prefix.start()]);
        output.push_str(prefix.as_str());
        output.push_str(&value);
        copied_until = inner.end();
        changed = true;
    }
    if changed {
        output.push_str(&raw[copied_until..]);
        Ok(Some(output))
    } else {
        Ok(None)
    }
}

fn decode_json_recursively(raw: &str) -> Result<Option<String>, String> {
    let mut current = raw.to_string();
    let mut changed = false;
    for _ in 0..128 {
        let Some(next) = decode_json_properties(&current)? else {
            break;
        };
        current = next;
        changed = true;
    }
    if changed && decode_json_properties(&current)?.is_some() {
        return Err("JSON decode nesting exceeds the safety limit".into());
    }
    Ok(changed.then_some(current))
}

pub fn decode(raw: &str) -> Result<Option<String>, String> {
    let xml_wrapper =
        regex::Regex::new(r"(?is)(<\s*auditData\b[^>]*>)(.*?)(<\s*/\s*auditData\s*>)")
            .map_err(|e| e.to_string())?;
    if let Some(parts) = xml_wrapper.captures(raw) {
        let inner = parts.get(2).ok_or("auditData has no content")?;
        if let Some(value) = decode_embedded_xml(inner.as_str())? {
            return Ok(Some(format!(
                "{}{}{}",
                &raw[..inner.start()],
                value,
                &raw[inner.end()..]
            )));
        }
        if let Some((value, _)) = decode_content(inner.as_str())? {
            return Ok(Some(format!(
                "{}{}{}",
                &raw[..inner.start()],
                value,
                &raw[inner.end()..]
            )));
        }
    }
    if let Some(value) = decode_json_recursively(raw)? {
        return Ok(Some(value));
    }
    if let Some((value, _)) = decode_content(raw.trim())? {
        let leading = &raw[..raw.len() - raw.trim_start().len()];
        let trailing = &raw[raw.trim_end().len()..];
        Ok(Some(format!("{leading}{value}{trailing}")))
    } else {
        Ok(None)
    }
}

fn decode_content(raw: &str) -> Result<Option<(String, Option<Kind>)>, String> {
    let raw = raw.replace('\u{2029}', "\n");
    let text = raw.trim();
    let mut candidates = vec![text.to_string()];
    for _ in 0..3 {
        for candidate in candidates.clone() {
            let mut next = vec![html_escape::decode_html_entities(&candidate).into_owned()];
            if let Ok(value) = serde_json::from_str::<String>(&candidate) {
                next.push(value);
            }
            next.push(
                candidate
                    .replace("\\/", "/")
                    .replace("\\\"", "\"")
                    .replace("\\n", "\n")
                    .replace("\\t", "\t")
                    .replace("\\r", "\r"),
            );
            for item in next {
                let item = item.trim().to_string();
                if !item.is_empty() && !candidates.contains(&item) {
                    candidates.push(item);
                }
            }
        }
    }
    for kind in [Kind::Json, Kind::Xml] {
        for candidate in &candidates {
            if let Ok(value) = format(candidate, kind)
                && value != text
            {
                return Ok(Some((value, Some(kind))));
            }
        }
    }
    Ok(candidates.into_iter().nth(1).map(|value| (value, None)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str, kind: Kind) -> Document {
        parse(Arc::from(text), kind, || false).unwrap()
    }

    #[test]
    fn json_ranges_include_repeated_keys_array_items_and_unicode() {
        let text = r#"{"é":"😀","a":[{"x":"\\"},{"x":2}],"1":true}"#;
        let doc = parsed(text, Kind::Json);
        let xs: Vec<_> = doc
            .nodes
            .iter()
            .filter(|n| n.label.starts_with("x:"))
            .collect();
        assert_eq!(xs.len(), 2);
        assert_eq!(&text[xs[1].range.clone()], r#""x":2"#);
        assert_eq!(
            text[..xs[1].range.start].chars().count() as i32,
            xs[1].chars.start
        );
        let item = doc
            .nodes
            .iter()
            .find(|n| n.label.starts_with("[1]:"))
            .unwrap();
        assert_eq!(&text[item.range.clone()], r#"{"x":2}"#);
    }

    #[test]
    fn xml_fragments_prefixes_self_closing_and_cdata() {
        let text =
            "\u{feff}<?xml version=\"1.0\"?><a><a/><a>é</a><![CDATA[<x>]]></a><p:b x=\"&amp;\"/>";
        let doc = parsed(text, Kind::Xml);
        assert_eq!(doc.roots.len(), 2);
        assert_eq!(&text[doc.nodes[1].range.clone()], "<a/>");
        assert_eq!(&text[doc.nodes[2].range.clone()], "<a>é</a>");
        assert!(doc.nodes.last().unwrap().label.contains("p:b"));
    }

    #[test]
    fn reject_invalid_documents() {
        for text in ["", "{\"a\":}", "[1,]", "{} garbage", r#"{"a":"\q"}"#] {
            assert!(parse(Arc::from(text), Kind::Json, || false).is_err());
        }
        for text in ["", "<a>", "<a></b>", "<a/>garbage", "<a x=\"1\" x=\"2\"/>"] {
            assert!(
                parse(Arc::from(text), Kind::Xml, || false).is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn formatting_preserves_json_tokens_and_xml_mixed_content() {
        let text = r#"{"a":1,"a":999999999999999999999999,"b":[{},[],true],"é":"😀"}"#;
        let formatted = format(text, Kind::Json).unwrap();
        assert_eq!(
            format("[[1],{\"x\":[]}]", Kind::Json).unwrap(),
            "[\n  [\n    1\n  ],\n  {\n    \"x\": []\n  }\n]"
        );
        assert!(formatted.contains("999999999999999999999999"));
        assert_eq!(
            parsed(&formatted, Kind::Json).nodes.len(),
            parsed(text, Kind::Json).nodes.len()
        );
        assert_eq!(format(&formatted, Kind::Json).unwrap(), formatted);
        let mixed = "<p>Hello <b>world</b> !</p>";
        assert_eq!(format(mixed, Kind::Xml).unwrap(), mixed);
        let xml = "<?xml version=\"1.0\"?><r><!--keep--><p:x xmlns:p=\"urn:x\"/></r>";
        let formatted = format(xml, Kind::Xml).unwrap();
        assert!(formatted.contains("<!--keep-->"));
        assert!(formatted.contains("p:x"));
        parsed(&formatted, Kind::Xml);
    }

    #[test]
    fn decoding_nested_audit_data_preserves_unicode() {
        let json = r#"{"auditData":"{\"é\":\"😀\"}"}"#;
        let decoded = decode(json).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(value["auditData"]["é"], "😀");
        let xml = "<auditData>&lt;r&gt;é&lt;/r&gt;</auditData>";
        assert!(decode(xml).unwrap().unwrap().contains("<r>é</r>"));
        assert!(decode("plain text").unwrap().is_none());
    }

    #[test]
    fn decoding_escaped_json_in_any_selected_property() {
        let selection = r#""data":"{\"result\":{\"ok\":true}}""#;
        let decoded = decode(selection).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_str(&format!("{{{decoded}}}")).unwrap();
        assert_eq!(value["data"]["result"]["ok"], true);
    }

    #[test]
    fn decoding_whole_json_skips_plain_strings_and_decodes_nested_payloads() {
        let json = r#"{"note":"keep me","auditData":{"request":"{\"id\":1}","response":"{\"data\":\"{\\\"ok\\\":true}\"}"}}"#;
        let decoded = decode(json).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(value["note"], "keep me");
        assert_eq!(value["auditData"]["request"]["id"], 1);
        assert_eq!(value["auditData"]["response"]["data"]["ok"], true);
    }

    #[test]
    fn decoding_legacy_audit_data_unescapes_embedded_xml_payloads() {
        let xml = "<audit><ValuationRS><auditData><transactions><RQ>postData:[&lt;?xml version=\"1.0\"?&gt;&lt;customer&gt;&lt;id&gt;123&lt;/id&gt;&lt;/customer&gt;]</RQ><RS>&lt;?xml version=\"1.0\"?&gt;&lt;result&gt;&lt;successful&gt;TRUE&lt;/successful&gt;&lt;/result&gt;</RS></transactions></auditData></ValuationRS></audit>";
        let decoded = decode(xml).unwrap().unwrap();
        assert!(decoded.contains("<customer>"));
        assert!(decoded.contains("<id>123</id>"));
        assert!(decoded.contains("<result>"));
        assert!(decoded.contains("<successful>TRUE</successful>"));
        parsed(&decoded, Kind::Xml);
    }

    #[test]
    fn cancellation_is_reported() {
        assert!(parse(Arc::from("[1,2]"), Kind::Json, || true).is_err());
    }

    #[test]
    fn json_with_byte_order_mark_keeps_character_offsets() {
        let text = "\u{feff}{\"a\":1}";
        let doc = parsed(text, Kind::Json);
        assert_eq!(doc.nodes[0].chars, 1..8);
        assert_eq!(&text[doc.nodes[1].range.clone()], "\"a\":1");
    }
}
