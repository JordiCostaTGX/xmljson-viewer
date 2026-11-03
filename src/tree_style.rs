use crate::document::{LabelStyle, ValueStyle};
use gtk::{gdk, pango};
use std::ops::Range;

const KEY: (u16, u16, u16) = (0x9c9c, 0xdcdc, 0xfefe);
const STRING: (u16, u16, u16) = (0xcece, 0x9191, 0x7878);
const NUMBER: (u16, u16, u16) = (0xb5b5, 0xcece, 0xa8a8);
const LITERAL: (u16, u16, u16) = (0x8282, 0xb8b8, 0xefef);
const ATTRIBUTE: (u16, u16, u16) = (0xc5c5, 0xa5a5, 0xefef);
const MUTED: (u16, u16, u16) = (0xabab, 0xb2b2, 0xbfbf);

const CSS: &str = r#"
listview.document-tree {
    background-color: #1e1e1e;
    color: #abb2bf;
}
listview.document-tree > row {
    padding: 4px 6px;
    border-radius: 4px;
    background-color: transparent;
}
listview.document-tree > row:hover {
    background-color: #292d36;
}
listview.document-tree > row:selected,
listview.document-tree > row:selected:hover {
    background-color: #253247;
    color: #abb2bf;
}
listview.document-tree > row:focus-visible {
    outline: 2px solid #82b8ef;
    outline-offset: -2px;
}
listview.document-tree .tree-node-label {
    font-family: monospace;
}
listview.document-tree treeexpander expander {
    color: #9cdcfe;
}
"#;

pub fn install(display: &gdk::Display) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn foreground(attrs: &pango::AttrList, range: Range<u32>, (red, green, blue): (u16, u16, u16)) {
    if range.is_empty() {
        return;
    }
    let mut attr = pango::AttrColor::new_foreground(red, green, blue);
    attr.set_start_index(range.start);
    attr.set_end_index(range.end);
    attrs.change(attr);
}

pub fn attributes(text: &str, style: Option<&LabelStyle>) -> pango::AttrList {
    let attrs = pango::AttrList::new();
    let end = text.len() as u32;
    foreground(&attrs, 0..end, MUTED);
    match style {
        Some(LabelStyle::Json { key_end, value }) => {
            let start = match key_end {
                Some(key_end) => {
                    foreground(&attrs, 0..*key_end, KEY);
                    *key_end + 2
                }
                None => 0,
            };
            let color = match value {
                ValueStyle::Container => MUTED,
                ValueStyle::String => STRING,
                ValueStyle::Number => NUMBER,
                ValueStyle::Literal => LITERAL,
            };
            foreground(&attrs, start..end, color);
        }
        Some(LabelStyle::Xml {
            element_end,
            attributes,
        }) => {
            foreground(&attrs, 1..*element_end, KEY);
            for attribute in attributes.iter() {
                foreground(&attrs, attribute.name.clone(), ATTRIBUTE);
                foreground(&attrs, attribute.value.clone(), STRING);
            }
        }
        Some(LabelStyle::Text) => foreground(&attrs, 0..end, STRING),
        Some(LabelStyle::Plain) | None => {}
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{self, Kind};
    use std::sync::Arc;

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
        let _ = attributes(&doc.nodes[1].label, Some(&doc.nodes[1].label_style));
        assert!(doc.nodes[1].label.contains("<span foreground='red'>"));
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
        let attrs = attributes(&doc.nodes[1].label, Some(&doc.nodes[1].label_style));
        let mut iterator = attrs.iterator();
        assert_eq!(iterator.range(), (0, 2));
        assert!(iterator.next_style_change());
        assert_eq!(iterator.range(), (2, 6));
        assert_eq!(attributes("Items 1–512", None).iterator().attrs().len(), 1);
    }

    #[test]
    fn palette_contrast_remains_readable_on_selected_rows() {
        fn luminance((r, g, b): (u16, u16, u16)) -> f64 {
            fn linear(value: u16) -> f64 {
                let value = f64::from(value) / 65535.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        }
        let selected = luminance((0x2525, 0x3232, 0x4747));
        for color in [KEY, STRING, NUMBER, LITERAL, ATTRIBUTE, MUTED] {
            assert!((luminance(color) + 0.05) / (selected + 0.05) >= 4.5);
        }
    }
}
