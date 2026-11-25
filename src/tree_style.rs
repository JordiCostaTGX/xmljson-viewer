use crate::{
    document::{LabelStyle, ValueStyle},
    theme::Palette,
};
use eframe::egui::{
    Color32, FontId,
    text::{LayoutJob, LayoutSection, TextFormat},
};
use std::ops::Range;

/// Colors per byte range; later ranges override earlier ones.
fn colors(
    text: &str,
    style: Option<&LabelStyle>,
    palette: &Palette,
) -> Vec<(Range<usize>, Color32)> {
    let end = text.len();
    let mut colors = vec![(0..end, palette.tree_muted)];
    match style {
        Some(LabelStyle::Json { key_end, value }) => {
            let start = match key_end {
                Some(key_end) => {
                    colors.push((0..*key_end as usize, palette.key));
                    *key_end as usize + 2
                }
                None => 0,
            };
            let color = match value {
                ValueStyle::Container => palette.tree_muted,
                ValueStyle::String => palette.string,
                ValueStyle::Number => palette.number,
                ValueStyle::Literal => palette.tree_literal,
            };
            colors.push((start.min(end)..end, color));
        }
        Some(LabelStyle::Xml {
            element_end,
            attributes,
        }) => {
            colors.push((1..*element_end as usize, palette.key));
            for attribute in attributes.iter() {
                colors.push((
                    attribute.name.start as usize..attribute.name.end as usize,
                    palette.tree_attribute,
                ));
                colors.push((
                    attribute.value.start as usize..attribute.value.end as usize,
                    palette.string,
                ));
            }
        }
        Some(LabelStyle::Text) => colors.push((0..end, palette.string)),
        Some(LabelStyle::Plain) | None => {}
    }
    colors
}

pub fn job(text: &str, style: Option<&LabelStyle>, font: FontId, palette: &Palette) -> LayoutJob {
    let mut per_byte = vec![palette.tree_muted; text.len()];
    for (range, color) in colors(text, style, palette) {
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
    use crate::theme::DARK;
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
            &DARK,
        );
        assert_eq!(job.text, *label);
        assert_eq!(sections(&job)[0], (0.."é:😀".len(), DARK.key));
        assert_eq!(sections(&job).last().unwrap().1, DARK.string);
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
            &DARK,
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
            &DARK,
        );
        assert_eq!(
            sections(&styled),
            [(0..2, DARK.tree_muted), (2..6, DARK.tree_literal)]
        );
        assert_eq!(
            job("Items 1–512", None, FontId::monospace(13.0), &DARK)
                .sections
                .len(),
            1
        );
    }
}
