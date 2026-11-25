use crate::syntax::Style;
use eframe::egui::{self, Color32, Visuals};

pub struct Palette {
    pub dark: bool,
    pub background: Color32,
    pub panel: Color32,
    pub text: Color32,
    pub line_number: Color32,
    pub current_line_number: Color32,
    pub current_line: Color32,
    pub selection: Color32,
    pub search_match: Color32,
    pub caret: Color32,
    pub tree_hover: Color32,
    pub tree_selected: Color32,
    pub key: Color32,
    pub string: Color32,
    pub number: Color32,
    pub keyword: Color32,
    pub comment: Color32,
    pub punctuation: Color32,
    pub tree_literal: Color32,
    pub tree_attribute: Color32,
    pub tree_muted: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const DARK: Palette = Palette {
    dark: true,
    background: rgb(0x1e1e1e),
    panel: rgb(0x1b1b1b),
    text: rgb(0xd4d4d4),
    line_number: rgb(0x858585),
    current_line_number: rgb(0xc6c6c6),
    current_line: rgb(0x282a2e),
    selection: rgb(0x264f78),
    search_match: rgb(0x613a14),
    caret: rgb(0xaeafad),
    tree_hover: rgb(0x292d36),
    tree_selected: rgb(0x253247),
    key: rgb(0x9cdcfe),
    string: rgb(0xce9178),
    number: rgb(0xb5cea8),
    keyword: rgb(0x569cd6),
    comment: rgb(0x6a9955),
    punctuation: rgb(0x808080),
    tree_literal: rgb(0x82b8ef),
    tree_attribute: rgb(0xc5a5ef),
    tree_muted: rgb(0xabb2bf),
};

const LIGHT: Palette = Palette {
    dark: false,
    background: rgb(0xffffff),
    panel: rgb(0xf3f3f3),
    text: rgb(0x1f1f1f),
    line_number: rgb(0x8c8c8c),
    current_line_number: rgb(0x0b216f),
    current_line: rgb(0xeef3fb),
    selection: rgb(0xadd6ff),
    search_match: rgb(0xffd78c),
    caret: rgb(0x000000),
    tree_hover: rgb(0xe8eef7),
    tree_selected: rgb(0xcfe0f7),
    key: rgb(0x0451a5),
    string: rgb(0xa31515),
    number: rgb(0x076f49),
    keyword: rgb(0x0000ff),
    comment: rgb(0x008000),
    punctuation: rgb(0x6e6e6e),
    tree_literal: rgb(0x0000ff),
    tree_attribute: rgb(0xc70000),
    tree_muted: rgb(0x5a5a5a),
};

const DRACULA: Palette = Palette {
    dark: true,
    background: rgb(0x282a36),
    panel: rgb(0x21222c),
    text: rgb(0xf8f8f2),
    line_number: rgb(0x6473a5),
    current_line_number: rgb(0xf8f8f2),
    current_line: rgb(0x313342),
    selection: rgb(0x44475a),
    search_match: rgb(0x5e4b35),
    caret: rgb(0xf8f8f2),
    tree_hover: rgb(0x343746),
    tree_selected: rgb(0x44475a),
    key: rgb(0x8be9fd),
    string: rgb(0xf1fa8c),
    number: rgb(0xcaa8fa),
    keyword: rgb(0xff79c6),
    comment: rgb(0x6473a5),
    punctuation: rgb(0xa0a4bb),
    tree_literal: rgb(0xff95d2),
    tree_attribute: rgb(0x50fa7b),
    tree_muted: rgb(0xbfc2d6),
};

const MONOKAI: Palette = Palette {
    dark: true,
    background: rgb(0x272822),
    panel: rgb(0x1e1f1c),
    text: rgb(0xf8f8f2),
    line_number: rgb(0x90908a),
    current_line_number: rgb(0xf8f8f2),
    current_line: rgb(0x3e3d32),
    selection: rgb(0x49483e),
    search_match: rgb(0x6b5a1e),
    caret: rgb(0xf8f8f0),
    tree_hover: rgb(0x3a3b33),
    tree_selected: rgb(0x49483e),
    key: rgb(0x66d9ef),
    string: rgb(0xe6db74),
    number: rgb(0xc7a8ff),
    keyword: rgb(0xfb7fac),
    comment: rgb(0x767260),
    punctuation: rgb(0xa09f93),
    tree_literal: rgb(0xc7a8ff),
    tree_attribute: rgb(0xa6e22e),
    tree_muted: rgb(0xcfcfc2),
};

const NORD: Palette = Palette {
    dark: true,
    background: rgb(0x2e3440),
    panel: rgb(0x272c36),
    text: rgb(0xd8dee9),
    line_number: rgb(0x727e95),
    current_line_number: rgb(0xd8dee9),
    current_line: rgb(0x3b4252),
    selection: rgb(0x434c5e),
    search_match: rgb(0x5c5a45),
    caret: rgb(0xd8dee9),
    tree_hover: rgb(0x3b4252),
    tree_selected: rgb(0x434c5e),
    key: rgb(0x9cc4c3),
    string: rgb(0xacc598),
    number: rgb(0xceb6ca),
    keyword: rgb(0x98b2cc),
    comment: rgb(0x727e95),
    punctuation: rgb(0x7b88a1),
    tree_literal: rgb(0xa9bfd5),
    tree_attribute: rgb(0xebcb8b),
    tree_muted: rgb(0xc0c8d8),
};

const SOLARIZED_LIGHT: Palette = Palette {
    dark: false,
    background: rgb(0xfdf6e3),
    panel: rgb(0xeee8d5),
    text: rgb(0x4f6369),
    line_number: rgb(0x838f8f),
    current_line_number: rgb(0x4f6369),
    current_line: rgb(0xf3ecd6),
    selection: rgb(0xe2dbc4),
    search_match: rgb(0xf2dd9b),
    caret: rgb(0x657b83),
    tree_hover: rgb(0xf0e9d4),
    tree_selected: rgb(0xe4ddc6),
    key: rgb(0x1c6599),
    string: rgb(0x1c6c66),
    number: rgb(0xad2c6b),
    keyword: rgb(0x627100),
    comment: rgb(0x838f8f),
    punctuation: rgb(0x839496),
    tree_literal: rgb(0xa83e12),
    tree_attribute: rgb(0x565a9d),
    tree_muted: rgb(0x52646a),
};

impl Palette {
    pub fn syntax(&self, style: Style) -> Color32 {
        match style {
            Style::Key | Style::Attribute => self.key,
            Style::String => self.string,
            Style::Number => self.number,
            Style::Literal | Style::Tag => self.keyword,
            Style::Comment => self.comment,
            Style::Punctuation => self.punctuation,
        }
    }

    pub fn visuals(&self) -> Visuals {
        let mut visuals = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        visuals.panel_fill = self.panel;
        visuals.window_fill = self.panel;
        visuals.extreme_bg_color = self.background;
        visuals.faint_bg_color = self.current_line;
        visuals.selection.bg_fill = self.selection;
        visuals.text_cursor.stroke.color = self.caret;
        visuals.widgets.noninteractive.fg_stroke.color = self.text;
        visuals.widgets.inactive.weak_bg_fill = self.tree_hover;
        visuals.widgets.inactive.bg_fill = self.tree_hover;
        visuals.widgets.hovered.weak_bg_fill = self.tree_selected;
        visuals.widgets.hovered.bg_fill = self.tree_selected;
        visuals
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Dark,
    Light,
    Dracula,
    Monokai,
    Nord,
    SolarizedLight,
}

impl Theme {
    pub const ALL: [Theme; 6] = [
        Theme::Dark,
        Theme::Light,
        Theme::Dracula,
        Theme::Monokai,
        Theme::Nord,
        Theme::SolarizedLight,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Dracula => "dracula",
            Theme::Monokai => "monokai",
            Theme::Nord => "nord",
            Theme::SolarizedLight => "solarized-light",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
            Theme::Dracula => "Dracula",
            Theme::Monokai => "Monokai",
            Theme::Nord => "Nord",
            Theme::SolarizedLight => "Solarized Light",
        }
    }

    /// Unknown ids, e.g. from a newer version, fall back to the default theme.
    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|theme| theme.id() == id)
            .unwrap_or_default()
    }

    pub fn palette(self) -> &'static Palette {
        match self {
            Theme::Dark => &DARK,
            Theme::Light => &LIGHT,
            Theme::Dracula => &DRACULA,
            Theme::Monokai => &MONOKAI,
            Theme::Nord => &NORD,
            Theme::SolarizedLight => &SOLARIZED_LIGHT,
        }
    }

    pub fn apply(self, ctx: &egui::Context) {
        let palette = self.palette();
        let theme = if palette.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(theme);
        ctx.set_visuals_of(theme, palette.visuals());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_unknown_ids_fall_back_to_dark() {
        for theme in Theme::ALL {
            assert_eq!(Theme::from_id(theme.id()), theme);
        }
        assert_eq!(Theme::from_id("neon"), Theme::Dark);
    }

    #[test]
    fn text_colors_keep_wcag_contrast_on_their_backgrounds() {
        fn luminance(color: Color32) -> f32 {
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.040_45 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }
        fn contrast(a: Color32, b: Color32) -> f32 {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }
        let mut failures = vec![];
        for theme in Theme::ALL {
            let p = theme.palette();
            let checks = [
                ("text", p.text, p.current_line, 4.5),
                ("key", p.key, p.current_line, 4.5),
                ("string", p.string, p.current_line, 4.5),
                ("number", p.number, p.current_line, 4.5),
                ("keyword", p.keyword, p.current_line, 4.5),
                ("text on selection", p.text, p.selection, 4.5),
                ("comment", p.comment, p.background, 3.0),
                ("line number", p.line_number, p.background, 3.0),
                ("tree key", p.key, p.tree_selected, 4.5),
                ("tree string", p.string, p.tree_selected, 4.5),
                ("tree number", p.number, p.tree_selected, 4.5),
                ("tree literal", p.tree_literal, p.tree_selected, 4.5),
                ("tree attribute", p.tree_attribute, p.tree_selected, 4.5),
                ("tree muted", p.tree_muted, p.tree_selected, 4.5),
            ];
            for (name, color, background, minimum) in checks {
                let ratio = contrast(color, background);
                if ratio < minimum {
                    failures.push(format!("{} {name}: {ratio:.2}", theme.name()));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
