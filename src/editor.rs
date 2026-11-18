use crate::{
    document::Kind,
    syntax::{self, State, Style},
};
use eframe::egui::{
    self, Align, Align2, Color32, Context, CursorIcon, Event, EventFilter, FontId, Galley, Id,
    ImeEvent, Key, Modifiers, Rect, ScrollArea, Sense, Ui, pos2,
    text::{CCursor, LayoutJob, TextFormat},
    vec2,
};
use ropey::Rope;
use std::{collections::HashMap, ops::Range, sync::Arc};

/// Lines longer than this are laid out in horizontal windows around the viewport.
const LONG_LINE: usize = 4096;
const WINDOW: usize = 1024;
const WINDOW_SPAN: usize = 4096;
const MAX_UNDO: usize = 1000;
const FONT_SIZE: f32 = 14.0;

pub const BACKGROUND: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
const TEXT: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd4);
const LINE_NUMBER: Color32 = Color32::from_rgb(0x85, 0x85, 0x85);
const CURRENT_LINE_NUMBER: Color32 = Color32::from_rgb(0xc6, 0xc6, 0xc6);
const CURRENT_LINE: Color32 = Color32::from_rgb(0x28, 0x2a, 0x2e);
const SELECTION: Color32 = Color32::from_rgb(0x26, 0x4f, 0x78);
const MATCH: Color32 = Color32::from_rgb(0x61, 0x3a, 0x14);
const CARET: Color32 = Color32::from_rgb(0xae, 0xaf, 0xad);

pub fn color(style: Style) -> Color32 {
    match style {
        Style::Key | Style::Attribute => Color32::from_rgb(0x9c, 0xdc, 0xfe),
        Style::String => Color32::from_rgb(0xce, 0x91, 0x78),
        Style::Number => Color32::from_rgb(0xb5, 0xce, 0xa8),
        Style::Literal | Style::Tag => Color32::from_rgb(0x56, 0x9c, 0xd6),
        Style::Comment => Color32::from_rgb(0x6a, 0x99, 0x55),
        Style::Punctuation => Color32::from_rgb(0x80, 0x80, 0x80),
    }
}

struct Edit {
    start: usize,
    removed: String,
    inserted: String,
}

struct Group {
    edits: Vec<Edit>,
    before: (usize, usize),
    after: (usize, usize),
    typing: bool,
}

struct Scroll {
    position: usize,
    center: bool,
}

#[derive(Clone)]
struct LineLayout {
    start: usize,
    len: usize,
    window: usize,
    count: usize,
    galley: Arc<Galley>,
}

pub struct Editor {
    rope: Rope,
    cursor: usize,
    anchor: usize,
    column: Option<usize>,
    undo: Vec<Group>,
    redo: Vec<Group>,
    sealed: bool,
    version: u64,
    pub editable: bool,
    kind: Option<Kind>,
    /// XML lexer state at the start of each line; only a valid prefix is kept.
    states: Vec<State>,
    /// May overestimate after deletions; it only sizes the horizontal scroll range.
    max_columns: usize,
    scroll: Option<Scroll>,
    layouts: HashMap<(usize, usize), LineLayout>,
    query: String,
    matches: Vec<Range<usize>>,
    matches_version: u64,
    page_rows: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            rope: Rope::new(),
            cursor: 0,
            anchor: 0,
            column: None,
            undo: vec![],
            redo: vec![],
            sealed: true,
            version: 0,
            editable: true,
            kind: None,
            states: vec![State::Text],
            max_columns: 0,
            scroll: None,
            layouts: HashMap::new(),
            query: String::new(),
            matches: vec![],
            matches_version: u64::MAX,
            page_rows: 20,
        }
    }
}

fn class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_alphanumeric() || ch == '_' {
        1
    } else {
        2
    }
}

fn digits(value: usize) -> usize {
    value.max(1).ilog10() as usize + 1
}

impl Editor {
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }

    pub fn selected_text(&self) -> String {
        self.rope.slice(self.selection()).to_string()
    }

    /// Zero-based line and column of the cursor.
    pub fn line_column(&self) -> (usize, usize) {
        let line = self.rope.char_to_line(self.cursor);
        (line, self.cursor - self.rope.line_to_char(line))
    }

    #[cfg(test)]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn set_kind(&mut self, kind: Kind) {
        if self.kind != Some(kind) {
            self.kind = Some(kind);
            self.states.truncate(1);
            self.layouts.clear();
        }
    }

    /// Replaces the whole text without recording undo history.
    pub fn set_text(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.undo.clear();
        self.redo.clear();
        self.cursor = 0;
        self.anchor = 0;
        self.column = None;
        self.sealed = true;
        self.changed(0);
        self.max_columns = (0..self.rope.len_lines())
            .map(|line| self.line_len(line))
            .max()
            .unwrap_or(0);
        self.scroll = Some(Scroll {
            position: 0,
            center: false,
        });
    }

    pub fn select_range(&mut self, start: usize, end: usize) {
        let len = self.len_chars();
        self.anchor = start.min(len);
        self.cursor = end.min(len);
        self.column = None;
        self.sealed = true;
        self.scroll = Some(Scroll {
            position: self.anchor,
            center: true,
        });
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.len_chars();
        self.sealed = true;
    }

    #[cfg(test)]
    pub fn place_cursor(&mut self, position: usize) {
        self.move_to(position, false);
    }

    /// Replaces a range as one undoable action and places the cursor after it.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let before = (self.anchor, self.cursor);
        let start = range.start.min(self.len_chars());
        let removed = self.apply(start, range.end, text);
        self.cursor = start + text.chars().count();
        self.anchor = self.cursor;
        self.column = None;
        self.sealed = true;
        self.push(Group {
            edits: vec![Edit {
                start,
                removed,
                inserted: text.into(),
            }],
            before,
            after: (self.anchor, self.cursor),
            typing: false,
        });
        self.reveal();
    }

    pub fn undo(&mut self) -> bool {
        let Some(group) = self.undo.pop() else {
            return false;
        };
        for edit in group.edits.iter().rev() {
            let end = edit.start + edit.inserted.chars().count();
            self.apply(edit.start, end, &edit.removed);
        }
        self.restore(group.before);
        self.redo.push(group);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(group) = self.redo.pop() else {
            return false;
        };
        for edit in &group.edits {
            let end = edit.start + edit.removed.chars().count();
            self.apply(edit.start, end, &edit.inserted);
        }
        self.restore(group.after);
        self.undo.push(group);
        true
    }

    pub fn set_search(&mut self, query: &str) {
        if self.query != query {
            self.query = query.into();
            self.matches_version = u64::MAX;
        }
    }

    pub fn match_count(&mut self) -> usize {
        self.ensure_matches();
        self.matches.len()
    }

    /// Selects the next or previous literal match, wrapping around.
    pub fn find(&mut self, backwards: bool) -> bool {
        self.ensure_matches();
        let selection = self.selection();
        let found = if backwards {
            let index = self.matches.partition_point(|m| m.start < selection.start);
            index
                .checked_sub(1)
                .and_then(|i| self.matches.get(i))
                .or(self.matches.last())
        } else {
            let index = self.matches.partition_point(|m| m.start < selection.end);
            self.matches.get(index).or(self.matches.first())
        };
        let Some(found) = found.cloned() else {
            return false;
        };
        self.select_range(found.start, found.end);
        true
    }

    fn ensure_matches(&mut self) {
        if self.matches_version == self.version {
            return;
        }
        self.matches_version = self.version;
        self.matches.clear();
        if self.query.is_empty() {
            return;
        }
        let text = self.rope.to_string();
        let length = self.query.chars().count();
        let (mut byte, mut chars) = (0, 0);
        for (at, _) in text.match_indices(self.query.as_str()) {
            chars += text[byte..at].chars().count();
            byte = at;
            self.matches.push(chars..chars + length);
        }
    }

    fn restore(&mut self, (anchor, cursor): (usize, usize)) {
        let len = self.len_chars();
        self.anchor = anchor.min(len);
        self.cursor = cursor.min(len);
        self.column = None;
        self.sealed = true;
        self.reveal();
    }

    fn push(&mut self, group: Group) {
        self.undo.push(group);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Low-level mutation; returns the removed text.
    fn apply(&mut self, start: usize, end: usize, text: &str) -> String {
        let len = self.len_chars();
        let start = start.min(len);
        let end = end.clamp(start, len);
        let removed = self.rope.slice(start..end).to_string();
        self.rope.remove(start..end);
        self.rope.insert(start, text);
        self.changed(start);
        let first = self.rope.char_to_line(start);
        let last = self.rope.char_to_line(start + text.chars().count());
        for line in first..=last {
            self.max_columns = self.max_columns.max(self.line_len(line));
        }
        removed
    }

    fn changed(&mut self, from: usize) {
        self.version += 1;
        let line = self.rope.char_to_line(from.min(self.len_chars()));
        self.states.truncate(line + 1);
        // Lines before the edit keep their text, offsets and lexer state.
        self.layouts.retain(|&(cached, _), _| cached < line);
    }

    fn type_text(&mut self, text: &str) {
        let range = self.selection();
        let before = (self.anchor, self.cursor);
        let multiline = text.contains(['\n', '\r']);
        let mergeable = !self.sealed
            && !multiline
            && range.is_empty()
            && self.undo.last().is_some_and(|group| {
                group.typing
                    && group.edits.len() == 1
                    && group.edits[0].start + group.edits[0].inserted.chars().count() == range.start
            });
        let removed = self.apply(range.start, range.end, text);
        self.cursor = range.start + text.chars().count();
        self.anchor = self.cursor;
        self.column = None;
        if mergeable {
            let group = self.undo.last_mut().expect("mergeable typing group");
            group.edits[0].inserted.push_str(text);
            group.after = (self.anchor, self.cursor);
        } else {
            self.push(Group {
                edits: vec![Edit {
                    start: range.start,
                    removed,
                    inserted: text.into(),
                }],
                before,
                after: (self.anchor, self.cursor),
                typing: !multiline,
            });
        }
        self.sealed = multiline;
        self.reveal();
    }

    fn reveal(&mut self) {
        self.scroll = Some(Scroll {
            position: self.cursor,
            center: false,
        });
    }

    fn move_to(&mut self, position: usize, extend: bool) {
        self.cursor = position.min(self.len_chars());
        if !extend {
            self.anchor = self.cursor;
        }
        self.column = None;
        self.sealed = true;
        self.reveal();
    }

    fn line_len(&self, line: usize) -> usize {
        let slice = self.rope.line(line);
        let mut len = slice.len_chars();
        if len > 0 && slice.char(len - 1) == '\n' {
            len -= 1;
        }
        if len > 0 && slice.char(len - 1) == '\r' {
            len -= 1;
        }
        len
    }

    fn previous(&self, at: usize) -> usize {
        if at >= 2 && self.rope.char(at - 1) == '\n' && self.rope.char(at - 2) == '\r' {
            at - 2
        } else {
            at.saturating_sub(1)
        }
    }

    fn next(&self, at: usize) -> usize {
        let len = self.len_chars();
        if at + 1 < len && self.rope.char(at) == '\r' && self.rope.char(at + 1) == '\n' {
            at + 2
        } else {
            (at + 1).min(len)
        }
    }

    fn word_left(&self, mut at: usize) -> usize {
        while at > 0 && class(self.rope.char(at - 1)) == 0 {
            at -= 1;
        }
        if at > 0 {
            let kind = class(self.rope.char(at - 1));
            while at > 0 && class(self.rope.char(at - 1)) == kind {
                at -= 1;
            }
        }
        at
    }

    fn word_right(&self, mut at: usize) -> usize {
        let len = self.len_chars();
        while at < len && class(self.rope.char(at)) == 0 {
            at += 1;
        }
        if at < len {
            let kind = class(self.rope.char(at));
            while at < len && class(self.rope.char(at)) == kind {
                at += 1;
            }
        }
        at
    }

    fn word_range(&self, at: usize) -> Range<usize> {
        let len = self.len_chars();
        if len == 0 {
            return 0..0;
        }
        let at = at.min(len - 1);
        let kind = class(self.rope.char(at));
        let (mut start, mut end) = (at, at);
        while start > 0 && class(self.rope.char(start - 1)) == kind {
            start -= 1;
        }
        while end < len && class(self.rope.char(end)) == kind {
            end += 1;
        }
        start..end
    }

    fn vertical(&mut self, delta: isize, extend: bool) {
        let (line, column) = self.line_column();
        let column = self.column.unwrap_or(column);
        let last = self.rope.len_lines().saturating_sub(1) as isize;
        let target = (line as isize + delta).clamp(0, last) as usize;
        let position = self.rope.line_to_char(target) + column.min(self.line_len(target));
        self.move_to(position, extend);
        self.column = Some(column);
    }

    fn delete(&mut self, range: Range<usize>) {
        if !range.is_empty() {
            self.replace(range, "");
        }
    }

    fn handle_events(&mut self, ctx: &Context) {
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            match event {
                Event::Text(text) | Event::Ime(ImeEvent::Commit(text))
                    if self.editable && !text.is_empty() =>
                {
                    self.type_text(&text);
                }
                Event::Copy | Event::Cut => {
                    let selection = self.selection();
                    if !selection.is_empty() {
                        ctx.copy_text(self.selected_text());
                        if matches!(event, Event::Cut) && self.editable {
                            self.delete(selection);
                        }
                    }
                }
                Event::Paste(text) if self.editable => self.replace(self.selection(), &text),
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => self.handle_key(key, modifiers),
                _ => {}
            }
        }
    }

    fn handle_key(&mut self, key: Key, modifiers: Modifiers) {
        let extend = modifiers.shift;
        let word = modifiers.ctrl || modifiers.alt;
        let selection = self.selection();
        let (line, _) = self.line_column();
        let line_start = self.rope.line_to_char(line);
        match key {
            Key::ArrowLeft if !extend && !selection.is_empty() => {
                self.move_to(selection.start, false)
            }
            Key::ArrowRight if !extend && !selection.is_empty() => {
                self.move_to(selection.end, false)
            }
            Key::ArrowLeft => {
                let target = if word {
                    self.word_left(self.cursor)
                } else {
                    self.previous(self.cursor)
                };
                self.move_to(target, extend);
            }
            Key::ArrowRight => {
                let target = if word {
                    self.word_right(self.cursor)
                } else {
                    self.next(self.cursor)
                };
                self.move_to(target, extend);
            }
            Key::ArrowUp => self.vertical(-1, extend),
            Key::ArrowDown => self.vertical(1, extend),
            Key::PageUp => self.vertical(-(self.page_rows as isize), extend),
            Key::PageDown => self.vertical(self.page_rows as isize, extend),
            Key::Home if modifiers.command => self.move_to(0, extend),
            Key::End if modifiers.command => self.move_to(self.len_chars(), extend),
            Key::Home => self.move_to(line_start, extend),
            Key::End => self.move_to(line_start + self.line_len(line), extend),
            Key::Backspace if self.editable => {
                if selection.is_empty() {
                    let start = if word {
                        self.word_left(self.cursor)
                    } else {
                        self.previous(self.cursor)
                    };
                    self.delete(start..self.cursor);
                } else {
                    self.delete(selection);
                }
            }
            Key::Delete if self.editable => {
                if selection.is_empty() {
                    let end = if word {
                        self.word_right(self.cursor)
                    } else {
                        self.next(self.cursor)
                    };
                    self.delete(self.cursor..end);
                } else {
                    self.delete(selection);
                }
            }
            Key::Enter if self.editable => {
                let line_text = self.rope.line(line);
                let indent: String = line_text
                    .chars()
                    .take(self.cursor - line_start)
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let crlf = line_text.len_chars() >= 2
                    && line_text.char(line_text.len_chars() - 2) == '\r'
                    && line_text.char(line_text.len_chars() - 1) == '\n';
                let newline = if crlf { "\r\n" } else { "\n" };
                self.replace(selection, &format!("{newline}{indent}"));
            }
            Key::Tab if self.editable && !modifiers.shift => self.type_text("  "),
            _ => {}
        }
    }

    fn state_at(&mut self, line: usize) -> State {
        while self.states.len() <= line {
            let previous = self.states.len() - 1;
            let start = self.rope.line_to_char(previous);
            let chars: Vec<char> = self
                .rope
                .slice(start..start + self.line_len(previous))
                .chars()
                .collect();
            let (_, next) = syntax::xml(&chars, self.states[previous]);
            self.states.push(next);
        }
        self.states[line]
    }

    fn window_for(&self, line: usize, column: usize) -> usize {
        if self.line_len(line) > LONG_LINE {
            column.saturating_sub(WINDOW / 2) / WINDOW * WINDOW
        } else {
            0
        }
    }

    fn line_layout(
        &mut self,
        ctx: &Context,
        line: usize,
        column: usize,
        font: &FontId,
    ) -> LineLayout {
        if self.layouts.len() > 1024 {
            self.layouts.clear();
        }
        let window = self.window_for(line, column);
        if let Some(layout) = self.layouts.get(&(line, window)) {
            return layout.clone();
        }
        let len = self.line_len(line);
        let start = self.rope.line_to_char(line);
        let end = if len > LONG_LINE {
            (window + WINDOW_SPAN).min(len)
        } else {
            len
        };
        // Tokens may begin before the window, so lex from the line start.
        let chars: Vec<char> = self.rope.slice(start..start + end).chars().collect();
        let spans = match self.kind {
            Some(Kind::Xml) => {
                let state = self.state_at(line);
                syntax::xml(&chars, state).0
            }
            Some(Kind::Json) => syntax::json(&chars),
            None => vec![],
        };
        let mut job = LayoutJob::default();
        let mut append = |from: usize, to: usize, color: Color32| {
            if from < to {
                let text: String = chars[from..to].iter().collect();
                job.append(&text, 0.0, TextFormat::simple(font.clone(), color));
            }
        };
        let mut at = window;
        for span in spans {
            if span.range.end <= at {
                continue;
            }
            if span.range.start >= end {
                break;
            }
            let from = span.range.start.max(at);
            let to = span.range.end.min(end);
            append(at, from, TEXT);
            append(from, to, color(span.style));
            at = to;
        }
        append(at, end, TEXT);
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        let layout = LineLayout {
            start,
            len,
            window,
            count: end.saturating_sub(window),
            galley,
        };
        self.layouts.insert((line, window), layout.clone());
        layout
    }

    fn x_of(layout: &LineLayout, column: usize, char_width: f32) -> f32 {
        let base = layout.window as f32 * char_width;
        if column < layout.window {
            column as f32 * char_width
        } else if column - layout.window <= layout.count {
            base + layout
                .galley
                .pos_from_cursor(CCursor::new(column - layout.window))
                .min
                .x
        } else {
            base + layout.galley.size().x
                + (column - layout.window - layout.count) as f32 * char_width
        }
    }

    fn column_at(layout: &LineLayout, x: f32, char_width: f32, row_height: f32) -> usize {
        let base = layout.window as f32 * char_width;
        let width = layout.galley.size().x;
        let column = if x < base {
            (x / char_width).round().max(0.0) as usize
        } else if x > base + width {
            layout.window + layout.count + ((x - base - width) / char_width).round() as usize
        } else {
            layout.window
                + layout
                    .galley
                    .cursor_from_pos(vec2(x - base, row_height * 0.5))
                    .index
                    .0
        };
        column.min(layout.len)
    }

    pub fn ui(&mut self, ui: &mut Ui, id: Id, request_focus: bool) {
        let ctx = ui.ctx().clone();
        let font = FontId::monospace(FONT_SIZE);
        let (row_height, char_width) =
            ctx.fonts_mut(|fonts| (fonts.row_height(&font), fonts.glyph_width(&font, 'M')));
        if request_focus {
            ctx.memory_mut(|memory| memory.request_focus(id));
        }
        let focused = ctx.memory(|memory| memory.has_focus(id));
        if focused {
            ctx.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                )
            });
            self.handle_events(&ctx);
        }
        self.ensure_matches();
        let lines = self.rope.len_lines();
        let gutter = (digits(lines).max(3) as f32 + 2.0) * char_width;
        let padding = char_width * 0.5;
        let size = vec2(
            gutter + padding + (self.max_columns + 2) as f32 * char_width,
            lines as f32 * row_height,
        );
        ui.painter()
            .rect_filled(ui.available_rect_before_wrap(), 0.0, BACKGROUND);
        ScrollArea::both()
            .id_salt(id)
            .auto_shrink(false)
            .content_margin(egui::Margin::ZERO)
            .show_viewport(ui, |ui, viewport| {
                let origin = ui.max_rect().min;
                let content = Rect::from_min_size(origin, size.max(viewport.size()));
                ui.allocate_rect(content, Sense::hover());
                // Interacting with the stable id keeps keyboard focus across frames.
                let response = ui.interact(content, id, Sense::click_and_drag());
                let clip = ui.clip_rect();
                let text_left = origin.x + gutter + padding;
                self.page_rows = ((viewport.height() / row_height) as usize).max(1);
                let first = ((viewport.min.y / row_height).floor().max(0.0) as usize)
                    .min(lines.saturating_sub(1));
                let last = ((viewport.max.y / row_height).ceil().max(0.0) as usize).min(lines);
                let column = ((viewport.min.x - gutter - padding) / char_width).max(0.0) as usize;

                if response.hovered() {
                    ctx.set_cursor_icon(CursorIcon::Text);
                }
                let pointer = ctx.input(|input| input.pointer.interact_pos());
                if let Some(pos) = pointer {
                    let pressed = ctx.input(|input| input.pointer.primary_pressed());
                    let (double, triple) = (response.double_clicked(), response.triple_clicked());
                    if (pressed && response.hovered()) || response.dragged() || double || triple {
                        let line = (((pos.y - origin.y) / row_height).floor().max(0.0) as usize)
                            .min(lines - 1);
                        let layout = self.line_layout(&ctx, line, column, &font);
                        let hit = layout.start
                            + Self::column_at(&layout, pos.x - text_left, char_width, row_height);
                        if triple {
                            self.anchor = layout.start;
                            self.cursor = layout.start + self.rope.line(line).len_chars();
                        } else if double {
                            let word = self.word_range(hit);
                            self.anchor = word.start;
                            self.cursor = word.end;
                        } else if pressed {
                            ctx.memory_mut(|memory| memory.request_focus(id));
                            let shift = ctx.input(|input| input.modifiers.shift);
                            self.cursor = hit;
                            if !shift {
                                self.anchor = hit;
                            }
                        } else {
                            self.cursor = hit;
                            self.reveal();
                        }
                        self.column = None;
                        self.sealed = true;
                    }
                }

                if let Some(scroll) = self.scroll.take() {
                    let position = scroll.position.min(self.len_chars());
                    let line = self.rope.char_to_line(position);
                    let offset = position - self.rope.line_to_char(line);
                    let layout = self.line_layout(&ctx, line, offset, &font);
                    let x = text_left + Self::x_of(&layout, offset, char_width);
                    let y = origin.y + line as f32 * row_height;
                    let target = Rect::from_min_max(
                        pos2(x - gutter - padding - char_width * 2.0, y),
                        pos2(x + char_width * 2.0, y + row_height),
                    );
                    ui.scroll_to_rect(target, scroll.center.then_some(Align::Center));
                    ctx.request_repaint();
                }

                let mut text_clip = clip;
                text_clip.min.x = clip.min.x + gutter;
                let painter = ui.painter().with_clip_rect(text_clip);
                let selection = self.selection();
                let (cursor_line, cursor_column) = self.line_column();
                let span_rect =
                    |layout: &LineLayout, y: f32, from: usize, to: usize, newline: bool| {
                        let x0 = text_left + Self::x_of(layout, from, char_width);
                        let mut x1 = text_left + Self::x_of(layout, to, char_width);
                        if newline {
                            x1 += char_width * 0.5;
                        }
                        Rect::from_min_max(pos2(x0, y), pos2(x1, y + row_height))
                    };
                for line in first..last {
                    let layout = self.line_layout(&ctx, line, column, &font);
                    let y = origin.y + line as f32 * row_height;
                    let (start, end) = (layout.start, layout.start + layout.len);
                    if line == cursor_line {
                        painter.rect_filled(
                            Rect::from_min_max(
                                pos2(clip.min.x, y),
                                pos2(clip.max.x, y + row_height),
                            ),
                            0.0,
                            CURRENT_LINE,
                        );
                    }
                    let first_match = self.matches.partition_point(|m| m.end <= start);
                    for found in self.matches[first_match..]
                        .iter()
                        .take_while(|m| m.start <= end)
                    {
                        let rect = span_rect(
                            &layout,
                            y,
                            found.start.max(start) - start,
                            found.end.min(end) - start,
                            false,
                        );
                        painter.rect_filled(rect, 2.0, MATCH);
                    }
                    if !selection.is_empty() && selection.start <= end && selection.end > start {
                        let rect = span_rect(
                            &layout,
                            y,
                            selection.start.max(start) - start,
                            selection.end.min(end) - start,
                            selection.end > end,
                        );
                        painter.rect_filled(rect, 0.0, SELECTION);
                    }
                    painter.galley(
                        pos2(text_left + layout.window as f32 * char_width, y),
                        layout.galley,
                        TEXT,
                    );
                }

                let caret_layout = self.line_layout(&ctx, cursor_line, cursor_column, &font);
                let caret_x = text_left + Self::x_of(&caret_layout, cursor_column, char_width);
                let caret_y = origin.y + cursor_line as f32 * row_height;
                let caret = Rect::from_min_max(
                    pos2(caret_x, caret_y),
                    pos2(caret_x + 2.0, caret_y + row_height),
                );
                if focused {
                    painter.rect_filled(caret, 0.0, CARET);
                    let to_global = ctx
                        .layer_transform_to_global(ui.layer_id())
                        .unwrap_or_default();
                    ctx.output_mut(|output| {
                        output.ime = Some(egui::output::IMEOutput {
                            purpose: egui::IMEPurpose::Normal,
                            rect: to_global * clip,
                            cursor_rect: to_global * caret,
                            should_interrupt_composition: false,
                        });
                    });
                }

                let gutter_rect =
                    Rect::from_min_max(clip.min, pos2(clip.min.x + gutter, clip.max.y));
                ui.painter().rect_filled(gutter_rect, 0.0, BACKGROUND);
                for line in first..last {
                    let y = origin.y + line as f32 * row_height + row_height * 0.5;
                    ui.painter().text(
                        pos2(clip.min.x + gutter - char_width, y),
                        Align2::RIGHT_CENTER,
                        line + 1,
                        font.clone(),
                        if line == cursor_line {
                            CURRENT_LINE_NUMBER
                        } else {
                            LINE_NUMBER
                        },
                    );
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut editor = Editor::default();
        editor.set_text(text);
        editor
    }

    #[test]
    fn typing_is_coalesced_and_undo_redo_restore_text() {
        let mut editor = editor("ab");
        editor.place_cursor(2);
        for ch in ["c", "d", "e"] {
            editor.type_text(ch);
        }
        assert_eq!(editor.text(), "abcde");
        assert!(editor.undo());
        assert_eq!(editor.text(), "ab");
        assert!(!editor.can_undo());
        assert!(editor.redo());
        assert_eq!(editor.text(), "abcde");
        assert_eq!(editor.cursor(), 5);
        editor.replace(0..2, "é😀");
        assert_eq!(editor.text(), "é😀cde");
        assert!(editor.undo());
        assert_eq!(editor.text(), "abcde");
    }

    #[test]
    fn crlf_is_one_cursor_step_and_enter_keeps_indentation() {
        let mut editor = editor("  a\r\nb");
        editor.place_cursor(3);
        assert_eq!(editor.next(3), 5);
        assert_eq!(editor.previous(5), 3);
        assert_eq!(editor.line_len(0), 3);
        editor.handle_key(Key::Enter, Modifiers::NONE);
        assert_eq!(editor.text(), "  a\r\n  \r\nb");
        editor.handle_key(Key::Backspace, Modifiers::NONE);
        assert_eq!(editor.text(), "  a\r\n \r\nb");
        editor.handle_key(Key::Backspace, Modifiers::CTRL);
        assert_eq!(editor.text(), "  \r\nb");
    }

    #[test]
    fn search_counts_unicode_matches_and_wraps() {
        let mut editor = editor("é x é x");
        editor.set_search("x");
        assert_eq!(editor.match_count(), 2);
        assert!(editor.find(false));
        assert_eq!(editor.selection(), 2..3);
        assert!(editor.find(false));
        assert_eq!(editor.selection(), 6..7);
        assert!(editor.find(false));
        assert_eq!(editor.selection(), 2..3);
        assert!(editor.find(true));
        assert_eq!(editor.selection(), 6..7);
        editor.replace(0..0, "x");
        assert_eq!(editor.match_count(), 3);
    }

    #[test]
    fn xml_states_are_invalidated_from_the_edited_line() {
        let mut editor = editor("<!--\na\n-->\n<b/>");
        editor.set_kind(Kind::Xml);
        assert_eq!(editor.state_at(3), State::Text);
        assert_eq!(editor.state_at(1), State::Comment);
        editor.replace(0..4, "<a>");
        assert_eq!(editor.state_at(1), State::Text);
        assert_eq!(editor.state_at(3), State::Text);
    }
}
