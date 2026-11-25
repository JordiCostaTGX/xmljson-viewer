use crate::{
    document::{Document, Kind, LabelStyle},
    theme::Palette,
    tree_style,
};
use eframe::egui::{
    Color32, EventFilter, FontId, Id, Key, Modifiers, Rect, ScrollArea, Sense, Shape, Stroke, Ui,
    pos2, vec2,
};
use std::{borrow::Cow, collections::HashSet, sync::Arc};

/// Branches with more children than this are split into expandable blocks.
pub const GROUP: usize = 512;
const INDENT: f32 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowKey {
    Node(usize),
    Group { parent: Option<usize>, index: usize },
}

pub struct Row {
    pub key: RowKey,
    pub depth: usize,
}

pub enum TreeAction {
    Select(usize),
    OpenEmbedded(usize),
}

#[derive(Default)]
pub struct Tree {
    document: Option<Arc<Document>>,
    expanded: HashSet<RowKey>,
    pub rows: Vec<Row>,
    pub selected: Option<RowKey>,
    scroll_to: Option<usize>,
    keep_visible: Option<usize>,
}

fn ids(document: &Document, parent: Option<usize>) -> &[usize] {
    match parent {
        Some(parent) => &document.nodes[parent].children,
        None => &document.roots,
    }
}

fn level(document: &Document, parent: Option<usize>) -> Vec<RowKey> {
    let ids = ids(document, parent);
    if ids.len() > GROUP {
        (0..ids.len().div_ceil(GROUP))
            .map(|index| RowKey::Group { parent, index })
            .collect()
    } else {
        ids.iter().map(|&id| RowKey::Node(id)).collect()
    }
}

fn children(document: &Document, key: RowKey) -> Vec<RowKey> {
    match key {
        RowKey::Node(id) => level(document, Some(id)),
        RowKey::Group { parent, index } => {
            let ids = ids(document, parent);
            ids[index * GROUP..((index + 1) * GROUP).min(ids.len())]
                .iter()
                .map(|&id| RowKey::Node(id))
                .collect()
        }
    }
}

impl Tree {
    pub fn set_document(&mut self, document: Arc<Document>) {
        self.expanded.clear();
        self.selected = None;
        self.scroll_to = None;
        self.keep_visible = None;
        // Expand only the first root; deeper branches stay collapsed.
        if let Some(&root) = document.roots.first() {
            self.expanded.insert(RowKey::Node(root));
        }
        self.document = Some(document);
        self.rebuild();
    }

    pub fn is_expanded(&self, key: RowKey) -> bool {
        self.expanded.contains(&key)
    }

    pub fn expandable(&self, key: RowKey) -> bool {
        match (key, &self.document) {
            (RowKey::Node(id), Some(document)) => !document.nodes[id].children.is_empty(),
            (RowKey::Group { .. }, _) => true,
            _ => false,
        }
    }

    pub fn toggle(&mut self, key: RowKey) {
        if self.expandable(key) && !self.expanded.insert(key) {
            self.expanded.remove(&key);
        }
        self.rebuild();
    }

    /// Expands every branch, including all blocks of large branches.
    pub fn expand_all(&mut self) {
        let Some(document) = self.document.clone() else {
            return;
        };
        let parents = std::iter::once(None).chain((0..document.nodes.len()).map(Some));
        for parent in parents {
            let count = ids(&document, parent).len();
            if let Some(id) = parent
                && count > 0
            {
                self.expanded.insert(RowKey::Node(id));
            }
            if count > GROUP {
                for index in 0..count.div_ceil(GROUP) {
                    self.expanded.insert(RowKey::Group { parent, index });
                }
            }
        }
        self.after_bulk_toggle();
    }

    /// Collapses everything except the roots, so their direct children stay visible.
    pub fn collapse_all(&mut self) {
        let Some(document) = self.document.clone() else {
            return;
        };
        self.expanded.clear();
        self.expanded
            .extend(document.roots.iter().map(|&root| RowKey::Node(root)));
        self.after_bulk_toggle();
    }

    fn after_bulk_toggle(&mut self) {
        self.rebuild();
        // Keep the selection in view, or go to the top if it is now hidden.
        self.scroll_to = Some(
            self.selected
                .and_then(|key| self.rows.iter().position(|row| row.key == key))
                .unwrap_or(0),
        );
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        let Some(document) = self.document.clone() else {
            return;
        };
        let mut stack: Vec<(RowKey, usize)> = level(&document, None)
            .into_iter()
            .rev()
            .map(|key| (key, 0))
            .collect();
        while let Some((key, depth)) = stack.pop() {
            self.rows.push(Row { key, depth });
            if self.expanded.contains(&key) {
                stack.extend(
                    children(&document, key)
                        .into_iter()
                        .rev()
                        .map(|child| (child, depth + 1)),
                );
            }
        }
    }

    /// Expands the ancestors (and enclosing blocks) of a node, selects it and scrolls to it.
    pub fn reveal(&mut self, id: usize, expand: bool) -> Option<usize> {
        let document = self.document.clone()?;
        let mut chain = vec![id];
        let mut parent = document.nodes[id].parent;
        while let Some(ancestor) = parent {
            chain.push(ancestor);
            parent = document.nodes[ancestor].parent;
        }
        chain.reverse();
        for (depth, &node) in chain.iter().enumerate() {
            let parent = document.nodes[node].parent;
            let siblings = ids(&document, parent);
            if siblings.len() > GROUP {
                let position = siblings.iter().position(|&x| x == node).unwrap_or(0);
                self.expanded.insert(RowKey::Group {
                    parent,
                    index: position / GROUP,
                });
            }
            if (depth + 1 < chain.len() || expand) && !document.nodes[node].children.is_empty() {
                self.expanded.insert(RowKey::Node(node));
            }
        }
        self.rebuild();
        self.selected = Some(RowKey::Node(id));
        self.scroll_to = self.rows.iter().position(|row| row.key == RowKey::Node(id));
        self.scroll_to
    }

    pub fn label(&self, key: RowKey) -> Cow<'_, str> {
        match (key, &self.document) {
            (RowKey::Node(id), Some(document)) => {
                let node = &document.nodes[id];
                if matches!(node.label_style, LabelStyle::Xml { .. }) && !node.children.is_empty() {
                    Cow::Owned(format!("{} ({})", node.label, node.children.len()))
                } else {
                    Cow::Borrowed(&node.label)
                }
            }
            (RowKey::Group { parent, index }, Some(document)) => {
                let total = ids(document, parent).len();
                Cow::Owned(format!(
                    "Items {}–{}",
                    index * GROUP + 1,
                    ((index + 1) * GROUP).min(total)
                ))
            }
            _ => Cow::Borrowed(""),
        }
    }

    fn select_row(&mut self, index: usize) -> Option<usize> {
        let key = self.rows.get(index)?.key;
        self.selected = Some(key);
        self.keep_visible = Some(index);
        match key {
            RowKey::Node(id) => Some(id),
            RowKey::Group { .. } => None,
        }
    }

    fn keyboard(&mut self, ui: &Ui, id: Id) -> Option<usize> {
        let ctx = ui.ctx();
        if !ui.is_enabled() || !ctx.memory(|memory| memory.has_focus(id)) {
            return None;
        }
        ctx.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..Default::default()
                },
            )
        });
        let [up, down, left, right, toggle] = ctx.input_mut(|input| {
            [
                Key::ArrowUp,
                Key::ArrowDown,
                Key::ArrowLeft,
                Key::ArrowRight,
                Key::Enter,
            ]
            .map(|key| input.consume_key(Modifiers::NONE, key))
        });
        let current = self
            .selected
            .and_then(|key| self.rows.iter().position(|row| row.key == key));
        let index = current.unwrap_or(0);
        let key = self.rows.get(index)?.key;
        if up {
            return self.select_row(index.saturating_sub(1));
        }
        if down {
            return self.select_row((index + 1).min(self.rows.len() - 1));
        }
        if toggle || (right && !self.is_expanded(key)) || (left && self.is_expanded(key)) {
            self.toggle(key);
            self.keep_visible = Some(index);
        } else if left {
            let depth = self.rows[index].depth;
            if let Some(parent) = self.rows[..index]
                .iter()
                .rposition(|row| row.depth + 1 == depth)
            {
                return self.select_row(parent);
            }
        }
        None
    }

    pub fn ui(&mut self, ui: &mut Ui, id: Id, palette: &Palette) -> Option<TreeAction> {
        ui.painter()
            .rect_filled(ui.available_rect_before_wrap(), 0.0, palette.background);
        // Rows have their own ids; this keeps keyboard focus on the tree itself.
        ui.interact(
            ui.available_rect_before_wrap(),
            id,
            Sense::focusable_noninteractive(),
        );
        let mut chosen = self.keyboard(ui, id).map(TreeAction::Select);
        let document = self.document.clone()?;
        let font = FontId::monospace(13.0);
        let row_height = ui.ctx().fonts_mut(|fonts| fonts.row_height(&font)) + 8.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = ScrollArea::vertical().id_salt(id).auto_shrink(false);
        if let Some(index) = self.scroll_to.take() {
            let offset = index as f32 * row_height - ui.available_height() / 2.0;
            area = area.vertical_scroll_offset(offset.max(0.0));
        }
        let enabled = ui.is_enabled();
        let mut toggled = None;
        let output = area.show_rows(ui, row_height, self.rows.len(), |ui, range| {
            for index in range {
                let row = Row {
                    key: self.rows[index].key,
                    depth: self.rows[index].depth,
                };
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::click());
                if self.keep_visible == Some(index) {
                    ui.scroll_to_rect(rect, None);
                    self.keep_visible = None;
                }
                let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                let fade = |color: Color32| {
                    if enabled {
                        color
                    } else {
                        color.gamma_multiply(0.5)
                    }
                };
                if self.selected == Some(row.key) {
                    painter.rect_filled(rect, 4.0, palette.tree_selected);
                } else if response.hovered() {
                    painter.rect_filled(rect, 4.0, palette.tree_hover);
                }
                let left = rect.left() + 6.0 + row.depth as f32 * INDENT;
                let center = rect.center().y;
                let expandable = self.expandable(row.key);
                if expandable {
                    let points = if self.is_expanded(row.key) {
                        vec![
                            pos2(left, center - 3.0),
                            pos2(left + 8.0, center - 3.0),
                            pos2(left + 4.0, center + 3.0),
                        ]
                    } else {
                        vec![
                            pos2(left + 2.0, center - 4.0),
                            pos2(left + 7.0, center),
                            pos2(left + 2.0, center + 4.0),
                        ]
                    };
                    painter.add(Shape::convex_polygon(
                        points,
                        fade(palette.key),
                        Stroke::NONE,
                    ));
                }
                let label = self.label(row.key);
                let style = match row.key {
                    RowKey::Node(node) => Some(&document.nodes[node].label_style),
                    RowKey::Group { .. } => None,
                };
                let mut job = tree_style::job(&label, style, font.clone(), palette);
                if !enabled {
                    for section in &mut job.sections {
                        section.format.color = fade(section.format.color);
                    }
                }
                let galley = painter.layout_job(job);
                painter.galley(
                    pos2(left + INDENT, center - galley.size().y / 2.0),
                    galley,
                    Color32::GRAY,
                );
                if enabled
                    && let RowKey::Node(node) = row.key
                    && let Some(kind) = document.nodes[node].embedded_kind()
                {
                    response.context_menu(|ui| {
                        let label = match kind {
                            Kind::Xml => "Open as XML",
                            Kind::Json => "Open as JSON",
                        };
                        if ui.button(label).clicked() {
                            chosen = Some(TreeAction::OpenEmbedded(node));
                            ui.close();
                        }
                    });
                }
                if response.clicked() {
                    ui.ctx().memory_mut(|memory| memory.request_focus(id));
                    let on_arrow = response
                        .interact_pointer_pos()
                        .is_some_and(|pos| pos.x < left + INDENT);
                    if !on_arrow {
                        self.selected = Some(row.key);
                        if let RowKey::Node(node) = row.key {
                            chosen = Some(TreeAction::Select(node));
                        }
                    }
                    if expandable {
                        toggled = Some(row.key);
                    }
                }
            }
        });
        let viewport = output.inner_rect;
        let rows_end = viewport.top() + output.content_size.y - output.state.clamped_offset().y;
        if rows_end < viewport.bottom() && !self.rows.is_empty() {
            let empty = Rect::from_min_max(pos2(viewport.left(), rows_end), viewport.max);
            if ui
                .interact(empty, id.with("empty"), Sense::click())
                .clicked()
            {
                ui.ctx().memory_mut(|memory| memory.request_focus(id));
                if let Some(node) = self.select_row(self.rows.len() - 1) {
                    chosen = Some(TreeAction::Select(node));
                }
            }
        }
        if let Some(index) = self.keep_visible.take() {
            self.scroll_to = Some(index);
            ui.ctx().request_repaint();
        }
        if let Some(key) = toggled {
            self.toggle(key);
        }
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document;

    fn tree(text: &str, kind: Kind) -> (Tree, Arc<Document>) {
        let document = Arc::new(document::parse(Arc::from(text), kind, || false).unwrap());
        let mut tree = Tree::default();
        tree.set_document(document.clone());
        (tree, document)
    }

    #[test]
    fn xml_labels_count_only_direct_children_and_leave_leaves_unchanged() {
        let (tree, document) = tree(
            "<hoteles><hotel><nombre/></hotel><hotel/></hoteles>",
            Kind::Xml,
        );
        let root = document.roots[0];
        let leaf = document.nodes[root].children[1];
        assert_eq!(tree.label(RowKey::Node(root)), "<hoteles> (2)");
        assert_eq!(tree.label(RowKey::Node(leaf)), "<hotel>");
    }

    #[test]
    fn json_labels_keep_their_existing_count_without_a_suffix() {
        let (tree, document) = tree(r#"{"hoteles":[{},{}]}"#, Kind::Json);
        let root = document.roots[0];
        let array = document.nodes[root].children[0];
        assert_eq!(tree.label(RowKey::Node(root)), "{1}");
        assert_eq!(tree.label(RowKey::Node(array)), "hoteles: [2]");
    }

    #[test]
    fn large_branches_are_grouped_and_reveal_opens_only_the_target_block() {
        let mut text = String::from("<r>");
        for id in 0..1100 {
            text.push_str(&format!("<e id=\"{id}\"/>"));
        }
        text.push_str("</r>");
        let (mut tree, document) = tree(&text, Kind::Xml);
        assert_eq!(tree.rows.len(), 4, "root plus three lazy groups");
        assert_eq!(tree.label(tree.rows[0].key), "<r> (1100)");
        assert_eq!(tree.label(tree.rows[3].key), "Items 1025–1100");
        let last = document.nodes.len() - 1;
        let index = tree.reveal(last, false).unwrap();
        assert_eq!(tree.rows[index].key, RowKey::Node(last));
        assert!(
            tree.rows.len() < 100,
            "only the target group is materialized"
        );
    }

    #[test]
    fn reveal_expands_ancestors_and_optionally_the_target() {
        let (mut tree, document) = tree(r#"{"a":[{"x":1},{"x":{"y":2}}]}"#, Kind::Json);
        let target = document
            .nodes
            .iter()
            .rposition(|n| n.label.starts_with("x:"))
            .unwrap();
        tree.reveal(target, false).unwrap();
        assert!(!tree.is_expanded(RowKey::Node(target)));
        tree.reveal(target, true).unwrap();
        assert!(tree.is_expanded(RowKey::Node(target)));
        let rows = tree.rows.len();
        tree.toggle(RowKey::Node(target));
        assert_eq!(tree.rows.len(), rows - 1);
    }

    #[test]
    fn expand_all_opens_every_branch_and_block_and_collapse_all_keeps_the_first_level() {
        let mut text = String::from("<r><a><b><c/></b></a><big>");
        for id in 0..600 {
            text.push_str(&format!("<e id=\"{id}\"/>"));
        }
        text.push_str("</big></r>");
        let (mut tree, document) = tree(&text, Kind::Xml);
        tree.expand_all();
        let groups = 600usize.div_ceil(GROUP);
        assert_eq!(tree.rows.len(), document.nodes.len() + groups);
        tree.collapse_all();
        let root = document.roots[0];
        assert_eq!(tree.rows.len(), 1 + document.nodes[root].children.len());
        assert!(tree.rows[1..].iter().all(|row| row.depth == 1));
    }

    #[test]
    fn clicking_below_the_rows_selects_the_last_row() {
        use eframe::egui::{Context, Event, PointerButton, RawInput};

        let (mut tree, document) = tree("<r><a/><b/></r>", Kind::Xml);
        let last = *document.nodes[document.roots[0]].children.last().unwrap();
        let ctx = Context::default();
        let id = Id::unique("tree");
        let pos = pos2(100.0, 500.0);
        let mut chosen = None;
        for pressed in [None, Some(true), Some(false)] {
            let mut events = vec![Event::PointerMoved(pos)];
            if let Some(pressed) = pressed {
                events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                });
            }
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 600.0))),
                events,
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                if let Some(action) = tree.ui(ui, id, &crate::theme::DARK) {
                    chosen = Some(action);
                }
            })
            .textures_delta
            .clear();
        }
        assert_eq!(tree.selected, Some(RowKey::Node(last)));
        assert!(matches!(chosen, Some(TreeAction::Select(node)) if node == last));
        assert!(ctx.memory(|memory| memory.has_focus(id)));
    }
}
