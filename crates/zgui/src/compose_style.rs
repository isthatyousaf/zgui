//! Styling lowered onto retained nodes; state changes preserve mounted children.
use crate::{
    input::{EventPhase, InputEvent, Key, PointerButton},
    reactive::Signal,
    scene::{Color, NodeId, NodeKind, QuadStyle},
    style::Styles,
    widgets::Ui,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Typography {
    pub display: crate::text_layout::TextOptions,
    pub color: Color,
    pub size: f32,
    pub wrap: bool,
    pub font: crate::text_layout::FontStyle,
}
/// Styles laid over a node's own while it is hovered, active, focused or
/// disabled. Boxed: most views set none, and every view carries the set.
#[derive(Default)]
pub(crate) struct Variants {
    pub hover: Option<Box<Styles>>,
    pub active: Option<Box<Styles>>,
    pub focus: Option<Box<Styles>>,
    pub disabled: Option<Box<Styles>>,
}
#[derive(Clone, Copy, PartialEq)]
enum ActivationKey {
    Space,
    Enter,
}
#[derive(Clone, Copy, Default, PartialEq)]
struct Interaction {
    hover: bool,
    pointer_active: bool,
    key_active: Option<ActivationKey>,
    focus: bool,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn mount_style(
    ui: &mut Ui,
    node: NodeId,
    base: Styles,
    inherited: Signal<Typography>,
    dynamic: Option<Box<dyn FnMut() -> Styles>>,
    variants: Variants,
    disabled: Option<Box<dyn FnMut() -> bool>>,
    interactive: bool,
) -> Signal<Typography> {
    mount_style_with_intrinsic(
        ui,
        node,
        base,
        inherited,
        dynamic,
        variants,
        disabled,
        interactive,
        None,
        None,
        None,
    )
}

/// Resolved styles, typography, intrinsic size and content layout target.
type AppliedInputs = (Styles, Typography, Option<(f32, f32)>, Option<NodeId>);
/// Paint properties an animation changes every frame: `Styles`' opacity,
/// blur, edge fade, transform and text colour.
#[derive(Clone, Copy, PartialEq)]
struct Paint {
    opacity: Option<f32>,
    blur: Option<f32>,
    edge_fade: Option<f32>,
    transform: Option<crate::scene::Transform>,
    text_color: Option<crate::scene::Color>,
}
impl Paint {
    /// Take `styles`' paint properties out, leaving them unset.
    fn take(styles: &mut Styles) -> Self {
        Self {
            opacity: styles.opacity.take(),
            blur: styles.blur.take(),
            edge_fade: styles.edge_fade.take(),
            transform: styles.transform.take(),
            text_color: styles.text_color.take(),
        }
    }
    fn put(self, styles: &mut Styles) {
        styles.opacity = self.opacity;
        styles.blur = self.blur;
        styles.edge_fade = self.edge_fade;
        styles.transform = self.transform;
        styles.text_color = self.text_color;
    }
    /// Which properties are set, whatever their values.
    fn set(&self) -> [bool; 5] {
        [
            self.opacity.is_some(),
            self.blur.is_some(),
            self.edge_fade.is_some(),
            self.transform.is_some(),
            self.text_color.is_some(),
        ]
    }
    /// `self` with what `over` sets laid over it, as `Styles::merge` does.
    fn over(self, over: Self) -> Self {
        Self {
            opacity: over.opacity.or(self.opacity),
            blur: over.blur.or(self.blur),
            edge_fade: over.edge_fade.or(self.edge_fade),
            transform: over.transform.or(self.transform),
            text_color: over.text_color.or(self.text_color),
        }
    }
    fn of(styles: &Styles) -> Self {
        Self {
            opacity: styles.opacity,
            blur: styles.blur,
            edge_fade: styles.edge_fade,
            transform: styles.transform,
            text_color: styles.text_color,
        }
    }
}
/// The inputs of the last full application besides paint: the dynamic
/// styles without their paint properties, the interaction and disabled
/// state, the inherited typography if it was read, the intrinsic size and
/// layout target. A run that matches them changes paint alone.
struct Settled {
    dynamic: Option<Styles>,
    paint: Option<[bool; 5]>,
    state: Interaction,
    disabled: bool,
    parent: Option<Typography>,
    intrinsic: Option<(f32, f32)>,
    content: Option<NodeId>,
}
/// What a re-run compares against: the inputs last applied to the scene
/// (unchanged results skip rebuilding and rewriting the node) and, for
/// paint-only changes, the last full application's other inputs.
struct Memo {
    applied: Option<AppliedInputs>,
    settled: Option<Settled>,
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn mount_style_with_intrinsic(
    ui: &mut Ui,
    node: NodeId,
    base: Styles,
    inherited: Signal<Typography>,
    mut dynamic: Option<Box<dyn FnMut() -> Styles>>,
    variants: Variants,
    mut disabled: Option<Box<dyn FnMut() -> bool>>,
    interactive: bool,
    intrinsic: Option<Signal<(f32, f32)>>,
    layout_target: Option<Signal<Option<NodeId>>>,
    object_fit: Option<Signal<crate::style::ObjectFit>>,
) -> Signal<Typography> {
    // A node setting no typography (nor through a variant or a dynamic
    // style) shows its parent's: it passes that signal on, not a copy.
    let own = dynamic.is_some()
        || base.sets_typography()
        || [&variants.hover, &variants.active, &variants.focus, &variants.disabled]
            .into_iter()
            .any(|variant| variant.as_deref().is_some_and(Styles::sets_typography));
    let typography = if own {
        ui.signal(inherited.with_untracked(Clone::clone))
    } else {
        inherited.clone()
    };
    let output = own.then(|| typography.clone());
    let interaction = ui.signal(Interaction::default());
    if interactive
        || variants.hover.is_some()
        || variants.active.is_some()
        || variants.focus.is_some()
    {
        let interaction = interaction.clone();
        ui.on_event(node, interactive, move |event| {
            if event.phase == EventPhase::Capture {
                return;
            }
            if event.default_prevented()
                && matches!(
                    event.event,
                    InputEvent::PointerDown { .. } | InputEvent::KeyDown { .. }
                )
            {
                return;
            }
            interaction.update(|state| match event.event {
                InputEvent::PointerEnter => state.hover = true,
                InputEvent::PointerLeave => {
                    state.hover = false;
                    state.pointer_active = false;
                }
                InputEvent::PointerDown {
                    button: PointerButton::Primary,
                    ..
                } => {
                    state.pointer_active = true;
                    if event.target == node {
                        event.capture_pointer();
                    }
                }
                InputEvent::PointerUp {
                    button: PointerButton::Primary,
                    ..
                }
                | InputEvent::PointerCancel => state.pointer_active = false,
                InputEvent::Focus if event.target == node => state.focus = true,
                InputEvent::Blur if event.target == node => {
                    state.focus = false;
                    state.pointer_active = false;
                    state.key_active = None;
                }
                InputEvent::KeyDown {
                    key: Key::Space,
                    repeat: false,
                    ..
                } if event.target == node => state.key_active = Some(ActivationKey::Space),
                InputEvent::KeyDown {
                    key: Key::Enter,
                    repeat: false,
                    ..
                } if event.target == node => state.key_active = Some(ActivationKey::Enter),
                InputEvent::KeyUp {
                    key: Key::Space, ..
                } if event.target == node && state.key_active == Some(ActivationKey::Space) => {
                    state.key_active = None
                }
                InputEvent::KeyUp {
                    key: Key::Enter, ..
                } if event.target == node && state.key_active == Some(ActivationKey::Enter) => {
                    state.key_active = None
                }
                _ => {}
            });
        });
    }
    let variants: [Option<Box<Styles>>; 4] = [
        variants.hover,
        variants.active,
        variants.focus,
        variants.disabled,
    ];
    let weak = ui.downgrade();
    let original_style = ui.scene.borrow().style(node);
    let original_effects = ui.scene.borrow().effects(node);
    let original_transform = ui.scene.borrow().transform(node);
    let original_isolated = ui.scene.borrow().is_isolated(node);
    let original_kind = ui.scene.borrow().kind(node).clone();
    let mut previous_disabled = None;
    // What re-runs compare against, kept from a node's second run: most
    // nodes are styled once, and each memo costs about 1.2 KB.
    let mut memo: Option<Box<Memo>> = None;
    let mut ran = false;
    ui.bind(node, move || {
        let Some(mut ui) = weak.upgrade() else { return };
        if !ui.scene.borrow().contains(node) { return; }
        if std::mem::replace(&mut ran, true) && memo.is_none() {
            memo = Some(Box::new(Memo { applied: None, settled: None }));
        }
        let mut produced = dynamic.as_mut().map(|dynamic| dynamic());
        let state = interaction.get();
        let is_disabled = disabled.as_mut().is_some_and(|read| read());
        let [hover, active, focus, disabled_variant] = &variants;
        let active_variants = [(!is_disabled && state.hover, hover), (!is_disabled && (state.pointer_active || state.key_active.is_some()), active), (!is_disabled && state.focus, focus), (is_disabled, disabled_variant)];
        // Animations re-run this every frame, mostly to change paint alone:
        // when nothing else differs from the last full application, update
        // just that, without resolving styles and typography again.
        if let Some(Memo { applied: Some(applied), settled: Some(last) }) = memo.as_deref_mut()
            && last.state == state
            && last.disabled == is_disabled
        {
            let paint = produced.as_mut().map(Paint::take);
            let unchanged = paint.map(|p| p.set()) == last.paint
                && produced == last.dynamic
                && last.parent.as_ref().is_none_or(|parent| inherited.with(|now| now == parent))
                && intrinsic.as_ref().map(|size| size.get()) == last.intrinsic
                && layout_target.as_ref().and_then(|target| target.get()) == last.content;
            if unchanged {
                let mut resolved = Paint::of(&base);
                if let Some(paint) = paint { resolved = resolved.over(paint); }
                for (enabled, variant) in active_variants {
                    if enabled && let Some(variant) = variant { resolved = resolved.over(Paint::of(variant)); }
                }
                let parent_color = match &last.parent {
                    Some(parent) => parent.color,
                    None => inherited.with_untracked(|parent| parent.color),
                };
                let color = resolved.text_color.unwrap_or(parent_color);
                resolved.put(&mut applied.0);
                let mut effects = original_effects;
                if let Some(value) = resolved.opacity { effects.opacity = value; }
                if let Some(value) = resolved.blur { effects.blur_radius = value; }
                if let Some(value) = resolved.edge_fade { effects.edge_fade = value; }
                if applied.1.color != color {
                    applied.1.color = color;
                    if let Some(output) = &output {
                        output.update(|typography| typography.color = color);
                    }
                }
                let mut scene = ui.scene.borrow_mut();
                scene.set_effects(node, effects);
                scene.set_transform(node, resolved.transform.unwrap_or(original_transform));
                if let NodeKind::Text { text, font_size, color: old } = scene.kind(node)
                    && *old != color
                {
                    let kind = NodeKind::Text { text: text.clone(), color, font_size: *font_size };
                    scene.set_kind(node, kind);
                }
                return;
            }
            if let (Some(produced), Some(paint)) = (&mut produced, paint) { paint.put(produced); }
        }
        let mut resolved = base.clone();
        if let Some(produced) = &produced { resolved.merge(produced); }
        for (enabled, variant) in active_variants {
            if enabled && let Some(variant) = variant { resolved.merge(variant); }
        }
        if disabled.is_some() && previous_disabled != Some(is_disabled) {
            previous_disabled = Some(is_disabled);
            ui.runtime.clone().untracked(|| ui.set_disabled(node, is_disabled));
        }
        if let Some(output) = &object_fit { output.set(resolved.object_fit.unwrap_or_default()); }
        // Only observe inherited typography when a field is actually inherited.
        let tracks_parent = resolved.rare().text_overflow.is_none() || resolved.rare().line_clamp.is_none() || resolved.text_color.is_none() || resolved.text_size.is_none() || resolved.text_wrap.is_none() || resolved.font_family.is_none() || resolved.rare().font_features.is_none() || resolved.text_align.is_none() || resolved.rare().font_fallbacks.is_none() || resolved.font_weight.is_none() || resolved.italic.is_none() || resolved.line_height.is_none() || resolved.rare().letter_spacing.is_none();
        let parent = if tracks_parent {
            inherited.get()
        } else {
            inherited.with_untracked(Clone::clone)
        };
        // Nothing of its own (the usual dynamic style: sizes, colours): the
        // parent's typography, as it is.
        let current = if !resolved.sets_typography() {
            parent.clone()
        } else {
            Typography { display: crate::text_layout::TextOptions {overflow: resolved.rare().text_overflow.unwrap_or(parent.display.overflow),line_clamp:resolved.rare().line_clamp.unwrap_or(parent.display.line_clamp)}, color: resolved.text_color.unwrap_or(parent.color), size: resolved.text_size.unwrap_or(parent.size), wrap: resolved.text_wrap.unwrap_or(parent.wrap), font: crate::text_layout::FontStyle { family: resolved.font_family.clone().unwrap_or_else(|| parent.font.family.clone()), features: resolved.rare().font_features.clone().unwrap_or_else(|| parent.font.features.clone()), align: resolved.text_align.unwrap_or(parent.font.align), fallbacks: resolved.rare().font_fallbacks.clone().unwrap_or_else(|| parent.font.fallbacks.clone()), weight: resolved.font_weight.unwrap_or(parent.font.weight), italic: resolved.italic.unwrap_or(parent.font.italic), line_height: resolved.line_height.unwrap_or(parent.font.line_height), letter_spacing: resolved.rare().letter_spacing.unwrap_or(parent.font.letter_spacing) } }
        };
        let tracked_parent = tracks_parent.then_some(parent);
        // Compare before cloning: unchanged typography notifies nothing.
        if let Some(output) = &output
            && output.with_untracked(|old| *old != current)
        {
            output.set(current.clone());
        }
        // Read every remaining dependency before any early return so the
        // effect keeps its subscriptions.
        let intrinsic_size = intrinsic.as_ref().map(|size| size.get());
        let content_layout = layout_target.as_ref().and_then(|target| target.get());
        if let Some(memo) = &mut memo {
            let mut dynamic_rest = produced;
            let paint = dynamic_rest.as_mut().map(|rest| Paint::take(rest).set());
            memo.settled = Some(Settled {
                dynamic: dynamic_rest,
                paint,
                state,
                disabled: is_disabled,
                parent: tracked_parent,
                intrinsic: intrinsic_size,
                content: content_layout,
            });
        }
        let inputs = (resolved, current, intrinsic_size, content_layout);
        if memo.as_ref().is_some_and(|memo| memo.applied.as_ref() == Some(&inputs)) {
            return;
        }
        // Animations mostly change paint effects or the transform, which need
        // neither layout nor a new paint kind: update just those.
        if let Some(Memo { applied: Some(previous), .. }) = memo.as_deref_mut()
            && previous.1 == inputs.1
            && previous.2 == inputs.2
            && previous.3 == inputs.3
        {
            let before = (previous.0.opacity, previous.0.blur, previous.0.edge_fade, previous.0.transform);
            let next = &inputs.0;
            (previous.0.opacity, previous.0.blur, previous.0.edge_fade, previous.0.transform) =
                (next.opacity, next.blur, next.edge_fade, next.transform);
            if previous.0 == *next {
                let mut effects = original_effects;
                if let Some(value) = next.opacity { effects.opacity = value; }
                if let Some(value) = next.blur { effects.blur_radius = value; }
                if let Some(value) = next.edge_fade { effects.edge_fade = value; }
                let mut scene = ui.scene.borrow_mut();
                scene.set_effects(node, effects);
                scene.set_transform(node, next.transform.unwrap_or(original_transform));
                return;
            }
            (previous.0.opacity, previous.0.blur, previous.0.edge_fade, previous.0.transform) = before;
        }
        let mut first: Option<AppliedInputs> = None;
        let (resolved, current, _, _) = match &mut memo {
            Some(memo) => &*memo.applied.insert(inputs),
            None => &*first.insert(inputs),
        };
        let mut style = original_style.clone();
        if let Some(resolved_options) = &resolved.layout_options {
            if let Some(original) = &style.layout_options {
                let mut options = **original;
                options.merge(**resolved_options);
                if options != **original { style.layout_options = Some(std::sync::Arc::new(options)); }
            } else {
                style.layout_options = Some(resolved_options.clone());
            }
        }
        macro_rules! field { ($($field:ident),* $(,)?) => { $(if let Some(value) = resolved.$field { style.$field = value; })* }; }
        let rare = resolved.rare();
        macro_rules! dimension { ($($field:ident),* $(,)?) => { $(if let Some(value) = rare.$field { style.$field = Some(value); })* }; }
        dimension!(min_width,max_width,min_height,max_height);
        if resolved.width.is_some() || resolved.width_percent.is_some() {
            style.width = resolved.width;
            style.width_percent = resolved.width_percent;
        }
        if resolved.height.is_some() || resolved.height_percent.is_some() {
            style.height = resolved.height;
            style.height_percent = resolved.height_percent;
        }
        field!(absolute,gap,clip,flex_grow,flex_shrink,align,justify,text_wrap,margin);
        if let Some(value) = rare.fade_edges { style.fade_edges = value; }
        if matches!(original_kind, NodeKind::Text { .. } | NodeKind::RichText { .. }) { style.text_wrap = current.wrap; style.text_options = current.display; }
        if let Some(padding) = resolved.padding { style.padding_edges = Some(padding); }
        if let Some((width, height)) = intrinsic_size {
            let padding = style.padding_edges.unwrap_or(crate::scene::Insets::all(style.padding));
            if resolved.width.is_none() && resolved.width_percent.is_none() { style.width = Some(width + padding.left.max(0.) + padding.right.max(0.)); }
            if resolved.height.is_none() && resolved.height_percent.is_none() { style.height = Some(height + padding.top.max(0.) + padding.bottom.max(0.)); }
        }

        let mut content_options = crate::layout::LayoutOptions::default();
        if layout_target.is_some() {
            style.gap = 0.; style.align = crate::scene::Align::Stretch; style.justify = crate::scene::Justify::Start;
            if let Some(options) = &mut style.layout_options {
                let options = std::sync::Arc::make_mut(options);
                if options.display != Some(crate::layout::Display::None) { content_options.display = options.display.take(); }
                content_options.wrap = options.wrap.take();
                content_options.reverse = options.reverse.take();
                content_options.align_content = options.align_content.take();
                content_options.gap_x = options.gap_x.take(); content_options.gap_y = options.gap_y.take();
                content_options.columns = options.columns.take(); content_options.rows = options.rows.take();
            }
        }
        let mut effects = original_effects;
        if let Some(value) = resolved.opacity { effects.opacity = value; }
        if let Some(value) = resolved.blur { effects.blur_radius = value; }
        if let Some(value) = resolved.edge_fade { effects.edge_fade = value; }
        let mut scene = ui.scene.borrow_mut();
        scene.set_style(node, style);
        if let Some(content) = content_layout {
            let mut content_style = scene.style(content);
            content_style.gap = resolved.gap.unwrap_or(0.);
            content_style.align = resolved.align.unwrap_or(crate::scene::Align::Stretch);
            content_style.justify = resolved.justify.unwrap_or(crate::scene::Justify::Start);
            content_style.layout_options = (!content_options.is_empty()).then(|| std::sync::Arc::new(content_options));
            scene.set_style(content, content_style);
            if let Some(layout) = resolved.layout { scene.set_kind(content, NodeKind::Container(layout)); }
        }

        scene.set_cursor(node, resolved.cursor);
        scene.set_effects(node, effects);
        scene.set_transform(node, resolved.transform.unwrap_or(original_transform));
        scene.set_isolated(node, resolved.isolated.unwrap_or(original_isolated));
        let kind = match &original_kind {
            NodeKind::Container(layout) | NodeKind::Panel { layout, .. } => {
                let mut quad = match &original_kind { NodeKind::Panel { quad, .. } => quad.clone(), _ => QuadStyle::default() };
                if let Some(value) = resolved.background { quad.fill = value; }
                if let Some(value) = resolved.radius { quad.radius = value; }
                if let Some(value) = resolved.border_color { quad.border_color = value; }
                if let Some(value) = resolved.border_width { quad.border_width = value; }
                if let Some(value) = resolved.rare().shadow { quad.shadow = value; }
                if quad.decoration.is_some() || resolved.paint_background.is_some() || resolved.corners.is_some() || resolved.border_edges.is_some() || resolved.rare().border_style.is_some() || resolved.rare().shadows.is_some() {
                    let mut detail = quad.decoration.as_deref().cloned().unwrap_or_default();
                    if let Some(value) = &resolved.paint_background { detail.background = value.clone(); }
                    if let Some(value) = resolved.corners { detail.corners = value; }
                    if let Some(value) = resolved.border_edges { detail.border_widths = value; }
                    if let Some(value) = resolved.rare().border_style { detail.border_style = value; }
                    if let Some(value) = &resolved.rare().shadows { detail.shadows = value.clone(); }
                    quad.decoration = (detail != crate::decoration::Decoration::default()).then(|| std::sync::Arc::new(detail));
                }
                if quad == QuadStyle::default() { NodeKind::Container(if layout_target.is_some() { *layout } else { resolved.layout.unwrap_or(*layout) }) }
                else { NodeKind::Panel { layout: if layout_target.is_some() { *layout } else { resolved.layout.unwrap_or(*layout) }, quad } }
            }
            NodeKind::Text { .. } => match scene.kind(node) {
                NodeKind::Text { text, .. } => NodeKind::Text { text: text.clone(), color: current.color, font_size: current.size },
                kind => kind.clone(),
            },
            _ => scene.kind(node).clone(),
        };
        if matches!(kind, NodeKind::Text { .. }) { scene.set_font(node, current.font.clone()); }
        scene.set_kind(node, kind);
    });
    typography
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Layout, Style};
    fn color(value: u8) -> Styles {
        Styles {
            background: Some(Color(value, 0, 0, 255)),
            ..Default::default()
        }
    }
    fn fill(ui: &Ui, node: NodeId) -> Color {
        match ui.scene.borrow().kind(node) {
            NodeKind::Panel { quad, .. } => quad.fill,
            _ => panic!("expected panel"),
        }
    }
    fn inherited(ui: &Ui) -> Signal<Typography> {
        ui.signal(Typography {
            display: Default::default(),
            color: Color(255, 255, 255, 255),
            size: 16.,
            wrap: false,
            font: Default::default(),
        })
    }
    #[test]
    fn interaction_variants_restore_and_disabled_wins_without_layout() {
        let mut ui = Ui::new(100., 100.);
        let node = ui.container(
            ui.root(),
            Layout::Column,
            Style {
                width: Some(50.),
                height: Some(50.),
                ..Default::default()
            },
        );
        let disabled = ui.signal(false);
        let read_disabled = disabled.clone();
        let inherited = inherited(&ui);
        mount_style(
            &mut ui,
            node,
            color(1),
            inherited,
            None,
            Variants {
                hover: Some(Box::new(color(2))),
                active: Some(Box::new(color(3))),
                focus: Some(Box::new(Styles {
                    border_width: Some(2.),
                    ..Default::default()
                })),
                disabled: Some(Box::new(color(4))),
            },
            Some(Box::new(move || read_disabled.get())),
            true,
        );
        ui.scene.borrow_mut().flush();
        ui.dispatch(InputEvent::PointerMove { x: 10., y: 10. });
        assert_eq!(fill(&ui, node), Color(2, 0, 0, 255));
        ui.dispatch(InputEvent::PointerDown {
            x: 10.,
            y: 10.,
            button: PointerButton::Primary,
        });
        assert_eq!(fill(&ui, node), Color(3, 0, 0, 255));
        assert_eq!(ui.input.focused(), Some(node));
        assert!(
            matches!(ui.scene.borrow().kind(node), NodeKind::Panel { quad, .. } if quad.border_width == 2.)
        );
        assert_eq!(ui.scene.borrow_mut().flush().layout_nodes, 0);
        disabled.set(true);
        assert_eq!(fill(&ui, node), Color(4, 0, 0, 255));
        assert_eq!(ui.input.focused(), None);
        assert!(
            matches!(ui.scene.borrow().kind(node), NodeKind::Panel { quad, .. } if quad.border_width == 0.)
        );
        assert!(ui.semantics.borrow().get(node).unwrap().disabled);
        disabled.set(false);
        assert_eq!(fill(&ui, node), Color(2, 0, 0, 255));
        ui.dispatch(InputEvent::PointerLeave);
        assert_eq!(fill(&ui, node), Color(1, 0, 0, 255));
    }
    #[test]
    fn typography_inherits_and_explicit_overrides_are_stable() {
        let mut ui = Ui::new(100., 100.);
        let parent = inherited(&ui);
        let text = ui.label(ui.root(), "retained", Style::default());
        let output = mount_style(
            &mut ui,
            text,
            Styles::default(),
            parent.clone(),
            None,
            Variants::default(),
            None,
            false,
        );
        let explicit = ui.label(ui.root(), "fixed", Style::default());
        let fixed = Typography {
            display: Default::default(),
            color: Color(12, 34, 56, 255),
            size: 20.,
            wrap: false,
            font: Default::default(),
        };
        let stable = mount_style(
            &mut ui,
            explicit,
            Styles {
                text_color: Some(fixed.color),
                text_size: Some(fixed.size),
                ..Default::default()
            },
            parent.clone(),
            None,
            Variants::default(),
            None,
            false,
        );
        ui.scene.borrow_mut().flush();
        parent.set(Typography {
            display: Default::default(),
            color: Color(1, 2, 3, 255),
            size: 16.,
            wrap: false,
            font: Default::default(),
        });
        assert_eq!(output.get().color, Color(1, 2, 3, 255));
        assert_eq!(stable.get(), fixed);
        assert_eq!(ui.scene.borrow_mut().flush().layout_nodes, 0);
        parent.set(Typography {
            display: Default::default(),
            color: Color(1, 2, 3, 255),
            size: 24.,
            wrap: false,
            font: Default::default(),
        });
        assert_eq!(output.get().size, 24.);
        assert_eq!(stable.get(), fixed);
        assert!(ui.scene.borrow_mut().flush().layout_nodes > 0);
    }
    #[test]
    fn removing_dynamic_properties_restores_base_and_inheritance() {
        let mut ui = Ui::new(100., 100.);
        let node = ui.container(ui.root(), Layout::Row, Style::default());
        let toggled = ui.signal(true);
        let read = toggled.clone();
        let parent = inherited(&ui);
        let output = mount_style(
            &mut ui,
            node,
            color(1),
            parent.clone(),
            Some(Box::new(move || {
                if read.get() {
                    Styles {
                        background: Some(Color(9, 0, 0, 255)),
                        text_size: Some(40.),
                        opacity: Some(0.5),
                        ..Default::default()
                    }
                } else {
                    Styles::default()
                }
            })),
            Variants::default(),
            None,
            false,
        );
        assert_eq!(output.get().size, 40.);
        assert_eq!(fill(&ui, node), Color(9, 0, 0, 255));
        toggled.set(false);
        assert_eq!(output.get().size, 16.);
        assert_eq!(fill(&ui, node), Color(1, 0, 0, 255));
        assert_eq!(ui.scene.borrow().effects(node).opacity, 1.);
        parent.set(Typography {
            display: Default::default(),
            size: 20.,
            ..parent.get()
        });
        assert_eq!(output.get().size, 20.);
    }
    #[test]
    fn styled_noninteractive_child_preserves_button_activation() {
        use crate::{
            compose::{button, text},
            style::Styled,
        };
        let mut ui = Ui::new(120., 80.);
        let clicks = ui.signal(0);
        let write = clicks.clone();
        ui.mount(
            button()
                .size(100., 60.)
                .child(text("Child").hover(|s| s.text_color(Color(255, 0, 0, 255))))
                .on_click(move || {
                    write.update(|count| *count += 1);
                }),
        );
        ui.dispatch(InputEvent::PointerMove { x: 15., y: 15. });
        ui.dispatch(InputEvent::PointerDown {
            x: 15.,
            y: 15.,
            button: PointerButton::Primary,
        });
        ui.dispatch(InputEvent::PointerUp {
            x: 15.,
            y: 15.,
            button: PointerButton::Primary,
        });
        assert_eq!(clicks.get(), 1);
    }
    #[test]
    fn nested_button_activation_stops_at_nearest_clickable_owner() {
        use crate::{
            compose::{button, text},
            style::Styled,
        };
        let mut ui = Ui::new(140., 100.);
        let outer = ui.signal(0);
        let inner = ui.signal(0);
        let outer_write = outer.clone();
        let inner_write = inner.clone();
        let mounted = ui.mount(
            button()
                .size(120., 80.)
                .p(0.)
                .on_click(move || {
                    outer_write.update(|n| *n += 1);
                })
                .child(
                    button()
                        .id("inner")
                        .size(80., 40.)
                        .on_click(move || {
                            inner_write.update(|n| *n += 1);
                        })
                        .child(text("Inner").hover(|s| s.opacity(0.8))),
                ),
        );
        ui.scene.borrow_mut().prepare_layout();
        let bounds = ui.scene.borrow().bounds(mounted.find("inner").unwrap());
        let x = bounds.x + 15.;
        let y = bounds.y + 15.;
        ui.dispatch(InputEvent::PointerMove { x, y });
        ui.dispatch(InputEvent::PointerDown {
            x,
            y,
            button: PointerButton::Primary,
        });
        ui.dispatch(InputEvent::PointerUp {
            x,
            y,
            button: PointerButton::Primary,
        });
        assert_eq!(inner.get(), 1);
        assert_eq!(outer.get(), 0);
    }
    #[test]
    fn unchanged_label_is_not_reshaped_beside_an_animating_sibling() {
        use crate::{
            compose::{column, div, row, text},
            style::Styled,
        };
        use std::{cell::Cell, rc::Rc};
        let mut ui = Ui::new(600., 400.);
        let calls = Rc::new(Cell::new(0_u32));
        let counter = calls.clone();
        ui.scene
            .borrow_mut()
            .set_text_measurer(move |text: &str, _: f32, _: Option<f32>| {
                if text == "MATRIX" {
                    counter.set(counter.get() + 1);
                }
                (text.len() as f32 * 10., 20.)
            });
        let side = ui.signal(10_f32);
        let read = side.clone();
        // A caption and a pulsing cell in a growing card, as in a loader tile.
        let stage = |label: &str, cell: crate::compose::View| {
            column()
                .grow()
                .min_w(0.)
                .h(150.)
                .p(16.)
                .gap(10.)
                .child(text(label))
                .child(
                    column()
                        .w_full()
                        .grow()
                        .items_center()
                        .justify_center()
                        .child(cell),
                )
        };
        ui.mount(
            column().w_full().gap(20.).child(text("Motion")).child(
                row()
                    .w_full()
                    .gap(12.)
                    .child(stage("MATRIX", div().size(30., 30.)))
                    .child(stage(
                        "WAVE",
                        div().reactive_style(move || Styles::new().size(read.get(), read.get())),
                    )),
            ),
        );
        ui.scene.borrow_mut().flush();
        let settled = calls.get();
        for step in 0..8 {
            side.set(10. + step as f32);
            ui.scene.borrow_mut().flush();
        }
        assert_eq!(
            calls.get(),
            settled,
            "MATRIX was reshaped {} times",
            calls.get() - settled
        );
    }
    #[test]
    fn animated_text_colour_repaints_without_layout() {
        use crate::{compose::text, style::Styled};
        let mut ui = Ui::new(300., 200.);
        ui.scene
            .borrow_mut()
            .set_text_measurer(|text: &str, size: f32, _: Option<f32>| {
                (text.len() as f32 * size, size)
            });
        let (alpha, size) = (ui.signal(90_u8), ui.signal(10_f32));
        let (read_alpha, read_size) = (alpha.clone(), size.clone());
        let view = ui.mount(text("shimmer").reactive_style(move || {
            Styles::new()
                .text_color(Color(255, 255, 255, read_alpha.get()))
                .text_size(read_size.get())
        }));
        ui.scene.borrow_mut().flush();
        // The text node, under whatever the view mounted around it.
        let mut node = view.node();
        while !matches!(ui.scene.borrow().kind(node), NodeKind::Text { .. }) {
            node = ui.scene.borrow().children(node)[0];
        }
        let shown = |ui: &Ui| match ui.scene.borrow().kind(node) {
            NodeKind::Text {
                text,
                color,
                font_size,
            } => (text.to_string(), *color, *font_size),
            kind => panic!("expected text, found {kind:?}"),
        };
        let style = ui.scene.borrow().style(node);
        alpha.set(200);
        let report = ui.scene.borrow_mut().flush();
        assert_eq!(
            shown(&ui),
            ("shimmer".into(), Color(255, 255, 255, 200), 10.)
        );
        assert_eq!(report.layout_nodes, 0);
        assert!(!report.damage.is_empty());
        assert_eq!(ui.scene.borrow().style(node), style);
        // A size change still lays out, and colour changes keep applying after it.
        size.set(20.);
        let report = ui.scene.borrow_mut().flush();
        assert!(report.layout_nodes > 0);
        assert_eq!(ui.scene.borrow().bounds(node).width, 140.);
        alpha.set(90);
        ui.scene.borrow_mut().flush();
        assert_eq!(
            shown(&ui),
            ("shimmer".into(), Color(255, 255, 255, 90), 20.)
        );
    }
    #[test]
    fn boxed_text_reactive_wrapping_reflows_without_remounting() {
        use crate::{compose::text, style::Styled};
        let mut ui = Ui::new(300., 200.);
        ui.scene
            .borrow_mut()
            .set_text_measurer(|text: &str, _: f32, limit: Option<f32>| {
                let natural = text.len() as f32 * 10.;
                let width = limit.unwrap_or(natural).min(natural).max(1.);
                (width, (natural / width).ceil() * 20.)
            });
        let wrap = ui.signal(false);
        let read = wrap.clone();
        let view = ui.mount(
            text("abcdefghijabcdefghij")
                .id("box")
                .w(70.)
                .p(10.)
                .bg(Color(1, 2, 3, 255))
                .text_wrap(false)
                .reactive_style(move || Styles::new().text_wrap(read.get())),
        );
        ui.scene.borrow_mut().flush();
        let outer = view.node();
        let inner = ui.scene.borrow().children(outer)[0];
        assert_eq!(ui.scene.borrow().bounds(outer).height, 40.);
        wrap.set(true);
        let report = ui.scene.borrow_mut().flush();
        assert!(report.layout_nodes > 0);
        assert_eq!(ui.scene.borrow().bounds(outer).height, 100.);
        assert_eq!(ui.scene.borrow().bounds(inner).width, 50.);
        assert_eq!(ui.scene.borrow().children(outer), &[inner]);
        assert_eq!(view.find("box"), Some(outer));
        wrap.set(false);
        ui.scene.borrow_mut().flush();
        assert_eq!(ui.scene.borrow().bounds(outer).height, 40.);
        assert_eq!(ui.scene.borrow().children(outer), &[inner]);
    }
    #[test]
    fn font_properties_inherit_override_and_update_without_remount() {
        use crate::{
            compose::{column, text},
            style::Styled,
            text_layout::FontFamily,
        };
        let mut ui = Ui::new(300., 200.);
        let bold = ui.signal(false);
        let read = bold.clone();
        let view = ui.mount(
            column()
                .font_family(FontFamily::Serif)
                .italic(true)
                .reactive_style(move || {
                    Styles::new().font_weight(if read.get() { 700 } else { 400 })
                })
                .child(text("inherited").id("a"))
                .child(
                    text("override")
                        .id("b")
                        .font_family(FontFamily::Monospace)
                        .font_weight(500)
                        .italic(false),
                ),
        );
        let a = view.find("a").unwrap();
        let b = view.find("b").unwrap();
        ui.scene.borrow_mut().flush();
        assert_eq!(ui.scene.borrow().font(a).family, FontFamily::Serif);
        assert!(ui.scene.borrow().font(a).italic);
        assert_eq!(ui.scene.borrow().font(b).weight, 500);
        assert!(!ui.scene.borrow().font(b).italic);
        bold.set(true);
        assert_eq!(view.find("a"), Some(a));
        assert_eq!(ui.scene.borrow().font(a).weight, 700);
        assert_eq!(ui.scene.borrow().font(b).weight, 500);
        assert!(ui.scene.borrow_mut().flush().layout_nodes > 0);
        bold.set(true);
        assert!(ui.scene.borrow_mut().flush().is_idle());
    }
}
