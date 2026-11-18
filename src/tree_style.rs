use crate::document::{LabelStyle, ValueStyle};
use eframe::egui::{
    Color32, FontId,
    text::{LayoutJob, LayoutSection, TextFormat},
};
use std::ops::Range;

pub const BACKGROUND: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
pub const HOVER: Color32 = Color32::from_rgb(0x29, 0x2d, 0x36);
pub const SELECTED: Color32 = Color32::from_rgb(0x25, 0x32, 0x47);
pub const EXPANDER: Color32 = KEY;
const KEY: Color32 = Color32::from_rgb(0x9c, 0xdc, 0xfe);
const STRING: Color32 = Color32::from_rgb(0xce, 0x91, 0x78);
const NUMBER: Color32 = Color32::from_rgb(0xb5, 0xce, 0xa8);
const LITERAL: Color32 = Color32::from_rgb(0x82, 0xb8, 0xef);
const ATTRIBUTE: Color32 = Color32::from_rgb(0xc5, 0xa5, 0xef);
const MUTED: Color32 = Color32::from_rgb(0xab, 0xb2, 0xbf);

/// Colors per byte range; later ranges override earlier ones.
fn colors(text: &str, style: Option<&LabelStyle>) -> Vec<(Range<usize>, Color32)> {
    let end = text.len();
    let mut colors = vec![(0..end, MUTED)];
    match style {
        Some(LabelStyle::Json { key_end, value }) => {
            let start = match key_end {
                Some(key_end) => {
                    colors.push((0..*key_end as usize, KEY));
                    *key_end as usize + 2
                }
                None => 0,
            };
            let color = match value {
                ValueStyle::Container => MUTED,
                ValueStyle::String => STRING,
                ValueStyle::Number => NUMBER,
                ValueStyle::Literal => LITERAL,
            };
            colors.push((start.min(end)..end, color));
        }
        Some(LabelStyle::Xml {
            element_end,
            attributes,
        }) => {
            colors.push((1..*element_end as usize, KEY));
            for attribute in attributes.iter() {
                colors.push((
                    attribute.name.start as usize..attribute.name.end as usize,
                    ATTRIBUTE,
                ));
                colors.push((
                    attribute.value.start as usize..attribute.value.end as usize,
                    STRING,
                ));
            }
        }
        Some(LabelStyle::Text) => colors.push((0..end, STRING)),
        Some(LabelStyle::Plain) | None => {}
    }
    colors
}

pub fn job(text: &str, style: Option<&LabelStyle>, font: FontId) -> LayoutJob {
    let mut per_byte = vec![MUTED; text.len()];
    for (range, color) in colors(text, style) {
        per_byte[range].fill(color);
    }
    let mut job = LayoutJob {
        text: text.into(),
        ..Default::default()
    };
    let mut start = 0;
    for at in 1..=text.len() {
        if at == text.len() || per_byte[at] != per_byte[start] {
            job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: start.into()..at.into(),
                format: TextFormat::simple(font.clone(), per_byte[start]),
            });
            start = at;
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{self, Kind};
    use std::sync::Arc;

    fn sections(job: &LayoutJob) -> Vec<(Range<usize>, Color32)> {
        job.sections
            .iter()
            .map(|s| (s.byte_range.start.0..s.byte_range.end.0, s.format.color))
            .collect()
    }

    #[test]
    fn json_styles_use_real_types_and_utf8_key_boundaries() {
        let text = r#"{"é:😀":"<span foreground='red'>","n":-1.2e3,"b":false,"nil":null,"a":[]}"#;
        let doc = document::parse(Arc::from(text), Kind::Json, || false).unwrap();
        let LabelStyle::Json { key_end, value } = &doc.nodes[1].label_style else {
            panic!("Expected JSON label style");
        };
        assert_eq!(key_end.unwrap() as usize, "é:😀".len());
        assert!(matches!(value, ValueStyle::String));
        assert!(matches!(
            doc.nodes[2].label_style,
            LabelStyle::Json {
                value: ValueStyle::Number,
                ..
            }
        ));
        for node in &doc.nodes[3..5] {
            assert!(matches!(
                node.label_style,
                LabelStyle::Json {
                    value: ValueStyle::Literal,
                    ..
                }
            ));
        }
        assert!(matches!(
            doc.nodes[5].label_style,
            LabelStyle::Json {
                value: ValueStyle::Container,
                ..
            }
        ));
        let label = &doc.nodes[1].label;
        let job = job(
            label,
            Some(&doc.nodes[1].label_style),
            FontId::monospace(13.0),
        );
        assert_eq!(job.text, *label);
        assert_eq!(sections(&job)[0], (0.."é:😀".len(), KEY));
        assert_eq!(sections(&job).last().unwrap().1, STRING);
    }

    #[test]
    fn xml_attribute_spans_survive_decoded_quotes_and_unicode() {
        let text =
            r#"<élément a="&quot; x=&quot;🦀" next="123">true<![CDATA[<b>&text]]></élément>"#;
        let doc = document::parse(Arc::from(text), Kind::Xml, || false).unwrap();
        let node = &doc.nodes[0];
        let LabelStyle::Xml {
            element_end,
            attributes,
        } = &node.label_style
        else {
            panic!("Expected XML label style");
        };
        assert_eq!(&node.label[1..*element_end as usize], "élément");
        assert_eq!(attributes.len(), 2);
        assert_eq!(
            &node.label[attributes[0].value.start as usize..attributes[0].value.end as usize],
            "\" x=\"🦀"
        );
        assert_eq!(
            &node.label[attributes[1].name.start as usize..attributes[1].name.end as usize],
            "next"
        );
        assert!(matches!(doc.nodes[1].label_style, LabelStyle::Text));
        assert!(matches!(doc.nodes[2].label_style, LabelStyle::Text));
        let job = job(
            &node.label,
            Some(&node.label_style),
            FontId::monospace(13.0),
        );
        let total: usize = sections(&job).iter().map(|s| s.0.len()).sum();
        assert_eq!(total, node.label.len());
    }

    #[test]
    fn empty_json_key_is_not_confused_with_an_unkeyed_root() {
        let doc = document::parse(Arc::from(r#"{"":true}"#), Kind::Json, || false).unwrap();
        assert!(matches!(
            doc.nodes[0].label_style,
            LabelStyle::Json { key_end: None, .. }
        ));
        assert!(matches!(
            doc.nodes[1].label_style,
            LabelStyle::Json {
                key_end: Some(0),
                ..
            }
        ));
        let styled = job(
            &doc.nodes[1].label,
            Some(&doc.nodes[1].label_style),
            FontId::monospace(13.0),
        );
        assert_eq!(sections(&styled), [(0..2, MUTED), (2..6, LITERAL)]);
        assert_eq!(
            job("Items 1–512", None, FontId::monospace(13.0))
                .sections
                .len(),
            1
        );
    }

    #[test]
    fn palette_contrast_remains_readable_on_selected_rows() {
        fn luminance(color: Color32) -> f64 {
            fn linear(value: u8) -> f64 {
                let value = f64::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
        }
        let selected = luminance(SELECTED);
        for color in [KEY, STRING, NUMBER, LITERAL, ATTRIBUTE, MUTED] {
            assert!((luminance(color) + 0.05) / (selected + 0.05) >= 4.5);
        }
    }
}
