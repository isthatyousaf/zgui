//! Display-only inline typography lowered to one retained paragraph.
use crate::{
    compose::View,
    scene::Color,
    style::{Styled, Styles},
    text_layout::{FontFamily, LetterSpacing, LineHeight},
};
use std::{cell::RefCell, ops::Range, rc::Rc, sync::Arc};
#[derive(Clone)]
pub(crate) struct InlineAction(Rc<RefCell<dyn FnMut()>>);
impl std::fmt::Debug for InlineAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InlineAction")
    }
}
impl PartialEq for InlineAction {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Link {
    pub range: Range<usize>,
    pub action: InlineAction,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Content {
    pub rich: Arc<crate::rich_text::RichText>,
    pub links: Vec<Link>,
}

/// An inline run. Only typography is supported; layout belongs to the paragraph.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpan {
    pub(crate) action: Option<InlineAction>,
    pub(crate) text: Arc<str>,
    pub(crate) styles: Styles,
    pub(crate) background: Option<Color>,
    pub(crate) underline: Option<crate::rich_text::Decoration>,
    pub(crate) strikethrough: Option<crate::rich_text::Decoration>,
}
pub fn text_span(text: impl Into<Arc<str>>) -> TextSpan {
    TextSpan {
        action: None,
        text: text.into(),
        styles: Styles::new(),
        background: None,
        underline: None,
        strikethrough: None,
    }
}
impl TextSpan {
    /// Make this span a keyboard-accessible link with shaped hit regions.
    pub fn on_click(mut self, callback: impl FnMut() + 'static) -> Self {
        self.action = Some(InlineAction(Rc::new(RefCell::new(callback))));
        self.underline.get_or_insert_with(Default::default);
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }
    pub fn underline(mut self, decoration: crate::rich_text::Decoration) -> Self {
        self.underline = Some(decoration);
        self
    }
    pub fn strikethrough(mut self, decoration: crate::rich_text::Decoration) -> Self {
        self.strikethrough = Some(decoration);
        self
    }
    pub fn text_color(mut self, color: Color) -> Self {
        self.styles.text_color = Some(color);
        self
    }
    pub fn text_size(mut self, size: f32) -> Self {
        self.styles.text_size = Some(if size.is_finite() && size > 0. {
            size
        } else {
            16.
        });
        self
    }
    pub fn font_fallbacks(mut self, families: impl Into<std::sync::Arc<[FontFamily]>>) -> Self {
        self.styles.rare_mut().font_fallbacks = Some(families.into());
        self
    }
    pub fn font_features(mut self, features: crate::text_layout::FontFeatures) -> Self {
        self.styles.rare_mut().font_features = Some(features);
        self
    }
    pub fn font_family(mut self, family: impl Into<FontFamily>) -> Self {
        self.styles.font_family = Some(family.into());
        self
    }
    pub fn font_weight(mut self, weight: u16) -> Self {
        self.styles.font_weight = Some(weight.clamp(1, 1000));
        self
    }
    pub fn italic(mut self, italic: bool) -> Self {
        self.styles.italic = Some(italic);
        self
    }
    pub fn font_bold(self) -> Self {
        self.font_weight(700)
    }
    pub fn line_height_normal(mut self) -> Self {
        self.styles.line_height = Some(LineHeight::NORMAL);
        self
    }
    pub fn line_height(mut self, px: f32) -> Self {
        self.styles.line_height = Some(LineHeight::px(px));
        self
    }
    pub fn letter_spacing(mut self, px: f32) -> Self {
        self.styles.rare_mut().letter_spacing = Some(LetterSpacing::px(px));
        self
    }
}
/// Builder for inline children. Conversion to View retains a single paragraph.
pub struct RichTextView {
    view: View,
    spans: Vec<TextSpan>,
}
pub fn rich_text() -> RichTextView {
    RichTextView {
        view: crate::compose::rich_text_signal(Vec::new),
        spans: Vec::new(),
    }
}
impl RichTextView {
    pub fn child(mut self, span: TextSpan) -> Self {
        self.spans.push(span);
        self
    }
    pub fn children(mut self, spans: impl IntoIterator<Item = TextSpan>) -> Self {
        self.spans.extend(spans);
        self
    }
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.view = self.view.id(id);
        self
    }
}
impl Styled for RichTextView {
    fn styles_mut(&mut self) -> &mut Styles {
        self.view.styles_mut()
    }
}
impl From<RichTextView> for View {
    fn from(builder: RichTextView) -> Self {
        crate::compose::with_rich_spans(builder.view, builder.spans)
    }
}

pub(crate) fn mount_links(
    ui: &mut crate::widgets::Ui,
    owner: crate::scene::NodeId,
    paragraph: crate::scene::NodeId,
    content: crate::reactive::Signal<Content>,
) {
    use crate::{
        input::{EventPhase, InputEvent},
        scene::{Layout, NodeId, NodeKind, QuadStyle, Rect, Style},
        semantics::{Role, SemanticNode},
    };
    use std::cell::Cell;
    struct Mounted {
        node: NodeId,
        fragments: Rc<RefCell<Vec<NodeId>>>,
        action: Rc<RefCell<InlineAction>>,
        focus: Rc<Cell<bool>>,
        hover: Rc<Cell<bool>>,
    }
    let bounds = ui.observe_bounds(paragraph);
    let owner_bounds = ui.observe_bounds(owner);
    let engine = ui.observe_text_geometry_revision();
    let weak = ui.downgrade();
    let mut mounted: Vec<Mounted> = Vec::new();
    let mut geometry_key = None;
    let mut geometry: Vec<Vec<Rect>> = Vec::new();
    ui.bind(owner, move || {
        let data = content.get();
        let bounds = bounds.get();
        let owner_bounds = owner_bounds.get();
        let engine = engine.get();
        let Some(mut ui) = weak.upgrade() else {
            return;
        };
        if !ui.scene.borrow().contains(owner) {
            return;
        }
        let key = (
            data.rich.clone(),
            bounds.width.to_bits(),
            bounds.height.to_bits(),
            engine,
            data.links
                .iter()
                .map(|l| l.range.clone())
                .collect::<Vec<_>>(),
        );
        if geometry_key.as_ref() != Some(&key) {
            geometry = if data.links.is_empty() {
                Vec::new()
            } else {
                let layout = ui
                    .scene
                    .borrow()
                    .shape_rich_text(&data.rich, Some(bounds.width));
                data.links
                    .iter()
                    .map(|link| {
                        layout
                            .selection(link.range.clone())
                            .into_iter()
                            .filter_map(|rect| {
                                rect.intersection(Rect::new(0., 0., bounds.width, bounds.height))
                            })
                            .filter(|r| r.width > 0. && r.height > 0.)
                            .collect()
                    })
                    .collect()
            };
            geometry_key = Some(key);
        }
        while mounted.len() > data.links.len() {
            let old = mounted.pop().unwrap();
            ui.remove(old.node);
            if !ui.scene.borrow().contains(owner) {
                return;
            }
        }
        for (index, link) in data.links.iter().enumerate() {
            if index == mounted.len() {
                let node = ui.container(
                    owner,
                    Layout::Overlay,
                    Style {
                        absolute: true,
                        ..Default::default()
                    },
                );
                let fragments = Rc::new(RefCell::new(Vec::new()));
                let action = Rc::new(RefCell::new(link.action.clone()));
                let focus = Rc::new(Cell::new(false));
                let hover = Rc::new(Cell::new(false));
                let (nodes, callback, focused, hovered) = (
                    fragments.clone(),
                    action.clone(),
                    focus.clone(),
                    hover.clone(),
                );
                let weak = ui.downgrade();
                ui.on_event(node, true, move |cx| {
                    if cx.phase == EventPhase::Capture
                        || (cx.default_prevented()
                            && !matches!(
                                cx.event,
                                InputEvent::Blur
                                    | InputEvent::PointerLeave
                                    | InputEvent::PointerCancel
                            ))
                    {
                        return;
                    }
                    match cx.event {
                        InputEvent::Activate => {
                            cx.stop_propagation();
                            let action = callback.borrow().clone();
                            (action.0.borrow_mut())();
                            return;
                        }
                        InputEvent::Focus => focused.set(true),
                        InputEvent::Blur => focused.set(false),
                        InputEvent::PointerEnter => hovered.set(true),
                        InputEvent::PointerLeave | InputEvent::PointerCancel => hovered.set(false),
                        _ => return,
                    }
                    let Some(ui) = weak.upgrade() else {
                        return;
                    };
                    let border = if focused.get() || hovered.get() {
                        1.
                    } else {
                        0.
                    };
                    let mut scene = ui.scene.borrow_mut();
                    if !scene.contains(node) {
                        return;
                    }
                    let quad = QuadStyle {
                        border_width: border,
                        border_color: ui.theme.accent,
                        ..Default::default()
                    };
                    scene.set_kind(
                        node,
                        NodeKind::Panel {
                            layout: Layout::Overlay,
                            quad: quad.clone(),
                        },
                    );
                    for id in nodes.borrow().iter().copied() {
                        if scene.contains(id) {
                            scene.set_kind(id, NodeKind::Quad(quad.clone()));
                        }
                    }
                });
                mounted.push(Mounted {
                    node,
                    fragments,
                    action,
                    focus,
                    hover,
                });
            }
            let entry = &mut mounted[index];
            let old = entry.action.replace(link.action.clone());
            drop(old);
            if !ui.scene.borrow().contains(owner) {
                return;
            }
            ui.semantics.borrow_mut().set(
                entry.node,
                SemanticNode::new(Role::Link, &data.rich.text()[link.range.clone()]),
            );
            let rects = &geometry[index];
            let first = rects.first().copied().unwrap_or_default();
            let mut style = ui.scene.borrow().style(entry.node);
            style.width = Some(first.width);
            style.height = Some(first.height);
            let owner_style = ui.scene.borrow().style(owner);
            let padding = owner_style
                .padding_edges
                .unwrap_or(crate::scene::Insets::all(owner_style.padding));
            style.margin.left = bounds.x - owner_bounds.x + first.x - padding.left;
            style.margin.top = bounds.y - owner_bounds.y + first.y - padding.top;
            ui.scene.borrow_mut().set_style(entry.node, style);
            ui.set_disabled(entry.node, rects.is_empty());
            if !ui.scene.borrow().contains(owner) {
                return;
            }
            let mut nodes = entry.fragments.borrow_mut();
            while nodes.len() > rects.len().saturating_sub(1) {
                ui.remove(nodes.pop().unwrap());
            }
            let quad = QuadStyle {
                border_width: if entry.focus.get() || entry.hover.get() {
                    1.
                } else {
                    0.
                },
                border_color: ui.theme.accent,
                ..Default::default()
            };
            for (i, rect) in rects.iter().skip(1).enumerate() {
                let style = Style {
                    absolute: true,
                    width: Some(rect.width),
                    height: Some(rect.height),
                    margin: crate::scene::Insets {
                        left: rect.x - first.x,
                        top: rect.y - first.y,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                if i == nodes.len() {
                    let child = ui.scene.borrow_mut().append(
                        entry.node,
                        NodeKind::Quad(quad.clone()),
                        style,
                    );
                    nodes.push(child);
                } else {
                    ui.scene.borrow_mut().set_style(nodes[i], style);
                }
            }
        }
    });
}
