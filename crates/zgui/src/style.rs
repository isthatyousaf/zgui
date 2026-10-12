//! Fluent, sparse styling for retained views.
//!
//! Dimensions, spacing, text sizes, blur and translations use logical pixels,
//! except explicit percentage dimension methods.
//! Style patches only replace explicitly supplied properties; chaining follows
//! last-write precedence, including individual padding and margin sides.
use crate::scene::{Align, BoxShadow, Color, Insets, Justify, Layout, Transform};

/// How image pixels fit the allocated padded content box. Alignment is centered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ObjectFit {
    /// Stretch independently along each axis (the default).
    #[default]
    Fill,
    /// Preserve aspect ratio and show the entire image.
    Contain,
    /// Preserve aspect ratio and crop to fill the content box.
    Cover,
    /// Keep intrinsic pixel dimensions, clipping excess content.
    None,
    /// Use intrinsic size unless shrinking is needed to contain the image.
    ScaleDown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Styles {
    pub(crate) layout_options: Option<std::sync::Arc<crate::layout::LayoutOptions>>,
    pub(crate) object_fit: Option<ObjectFit>,
    pub(crate) layout: Option<Layout>,
    pub(crate) absolute: Option<bool>,
    pub(crate) width: Option<f32>,
    pub(crate) height: Option<f32>,
    pub(crate) width_percent: Option<f32>,
    pub(crate) height_percent: Option<f32>,
    pub(crate) padding: Option<Insets>,
    pub(crate) margin: Option<Insets>,
    pub(crate) gap: Option<f32>,
    pub(crate) clip: Option<bool>,
    pub(crate) flex_grow: Option<f32>,
    pub(crate) flex_shrink: Option<f32>,
    pub(crate) align: Option<Align>,
    pub(crate) justify: Option<Justify>,
    pub(crate) text_wrap: Option<bool>,
    pub(crate) background: Option<Color>,
    pub(crate) cursor: Option<crate::cursor::Cursor>,
    pub(crate) text_color: Option<Color>,
    pub(crate) text_size: Option<f32>,
    pub(crate) line_height: Option<crate::text_layout::LineHeight>,
    pub(crate) font_family: Option<crate::text_layout::FontFamily>,
    pub(crate) text_align: Option<crate::text_layout::TextAlign>,
    pub(crate) font_weight: Option<u16>,
    pub(crate) italic: Option<bool>,
    pub(crate) radius: Option<f32>,
    pub(crate) border_width: Option<f32>,
    pub(crate) border_color: Option<Color>,
    pub(crate) opacity: Option<f32>,
    pub(crate) blur: Option<f32>,
    pub(crate) edge_fade: Option<f32>,
    pub(crate) transform: Option<Transform>,
    pub(crate) isolated: Option<bool>,
    pub(crate) padding_sides: u8,
    pub(crate) margin_sides: u8,
    /// Set (to none) by `bg`, `rounded` and `border`, so on most nodes.
    pub(crate) paint_background: Option<Option<crate::decoration::Background>>,
    pub(crate) corners: Option<Option<crate::decoration::Corners>>,
    pub(crate) border_edges: Option<Option<Insets>>,
    /// Properties few views set (see `RareStyles`).
    pub(crate) rare: Option<std::sync::Arc<RareStyles>>,
}
/// Properties few views set, kept apart and shared so that every `Styles`
/// (a view's, its node's style effect's and that effect's memo) stays small:
/// a `Styles` is copied and compared on every re-run.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RareStyles {
    pub(crate) min_width: Option<f32>,
    pub(crate) max_width: Option<f32>,
    pub(crate) min_height: Option<f32>,
    pub(crate) max_height: Option<f32>,
    pub(crate) fade_edges: Option<[f32; 2]>,
    pub(crate) border_style: Option<crate::decoration::BorderStyle>,
    pub(crate) shadows: Option<Option<std::sync::Arc<[BoxShadow]>>>,
    pub(crate) shadow: Option<Option<BoxShadow>>,
    pub(crate) letter_spacing: Option<crate::text_layout::LetterSpacing>,
    pub(crate) text_overflow: Option<crate::text_layout::TextOverflow>,
    pub(crate) line_clamp: Option<Option<std::num::NonZeroU32>>,
    pub(crate) font_features: Option<crate::text_layout::FontFeatures>,
    pub(crate) font_fallbacks: Option<std::sync::Arc<[crate::text_layout::FontFamily]>>,
}
impl RareStyles {
    const NONE: Self = Self {
        min_width: None,
        max_width: None,
        min_height: None,
        max_height: None,
        fade_edges: None,
        border_style: None,
        shadows: None,
        shadow: None,
        letter_spacing: None,
        text_overflow: None,
        line_clamp: None,
        font_features: None,
        font_fallbacks: None,
    };
    /// Overlay explicitly supplied properties, preserving all others.
    fn merge(&mut self, other: &Self) {
        if other.min_width.is_some() {
            self.min_width = other.min_width;
        }
        if other.max_width.is_some() {
            self.max_width = other.max_width;
        }
        if other.min_height.is_some() {
            self.min_height = other.min_height;
        }
        if other.max_height.is_some() {
            self.max_height = other.max_height;
        }
        if other.fade_edges.is_some() {
            self.fade_edges = other.fade_edges;
        }
        if other.border_style.is_some() {
            self.border_style = other.border_style;
        }
        if other.shadows.is_some() {
            self.shadows = other.shadows.clone();
        }
        if other.shadow.is_some() {
            self.shadow = other.shadow;
        }
        if other.letter_spacing.is_some() {
            self.letter_spacing = other.letter_spacing;
        }
        if other.text_overflow.is_some() {
            self.text_overflow = other.text_overflow;
        }
        if other.line_clamp.is_some() {
            self.line_clamp = other.line_clamp;
        }
        if other.font_features.is_some() {
            self.font_features = other.font_features.clone();
        }
        if other.font_fallbacks.is_some() {
            self.font_fallbacks = other.font_fallbacks.clone();
        }
    }
}
static NO_RARE: RareStyles = RareStyles::NONE;
impl Styles {
    pub(crate) fn rare(&self) -> &RareStyles {
        self.rare.as_deref().unwrap_or(&NO_RARE)
    }
    pub(crate) fn rare_mut(&mut self) -> &mut RareStyles {
        std::sync::Arc::make_mut(self.rare.get_or_insert_with(Default::default))
    }
    /// Whether any typography field (what text inherits) is set.
    pub(crate) fn sets_typography(&self) -> bool {
        self.rare().text_overflow.is_some()
            || self.rare().line_clamp.is_some()
            || self.text_color.is_some()
            || self.text_size.is_some()
            || self.text_wrap.is_some()
            || self.font_family.is_some()
            || self.rare().font_features.is_some()
            || self.text_align.is_some()
            || self.rare().font_fallbacks.is_some()
            || self.font_weight.is_some()
            || self.italic.is_some()
            || self.line_height.is_some()
            || self.rare().letter_spacing.is_some()
    }
    pub(crate) fn layout_options_mut(&mut self) -> &mut crate::layout::LayoutOptions {
        std::sync::Arc::make_mut(self.layout_options.get_or_insert_with(Default::default))
    }
    fn clear_layout_spacing(&mut self, margin: bool, mask: u8) {
        let Some(options) = &mut self.layout_options else {
            return;
        };
        let options = std::sync::Arc::make_mut(options);
        let edges = if margin {
            &mut options.margin
        } else {
            &mut options.padding
        };
        if mask & 1 != 0 {
            edges.top = None;
        }
        if mask & 2 != 0 {
            edges.right = None;
        }
        if mask & 4 != 0 {
            edges.bottom = None;
        }
        if mask & 8 != 0 {
            edges.left = None;
        }
        if options.is_empty() {
            self.layout_options = None;
        }
    }
    pub fn new() -> Self {
        Self::default()
    }
    /// Overlay explicitly supplied properties, preserving all others.
    pub fn merge(&mut self, other: &Self) {
        if other.padding.is_some() {
            self.clear_layout_spacing(
                false,
                if other.padding_sides == 0 {
                    15
                } else {
                    other.padding_sides
                },
            );
        }
        if other.margin.is_some() {
            self.clear_layout_spacing(
                true,
                if other.margin_sides == 0 {
                    15
                } else {
                    other.margin_sides
                },
            );
        }
        if let Some(options) = &mut self.layout_options
            && (other.gap.is_some()
                || other.clip.is_some()
                || other.rare().min_width.is_some()
                || other.rare().max_width.is_some()
                || other.rare().min_height.is_some()
                || other.rare().max_height.is_some()
                || other.layout.is_some())
        {
            let options = std::sync::Arc::make_mut(options);
            if other.gap.is_some() {
                options.gap_x = None;
                options.gap_y = None;
            }
            if other.clip.is_some() {
                options.clip_x = None;
                options.clip_y = None;
            }
            if other.rare().min_width.is_some() {
                options.min_width = None;
            }
            if other.rare().max_width.is_some() {
                options.max_width = None;
            }
            if other.rare().min_height.is_some() {
                options.min_height = None;
            }
            if other.rare().max_height.is_some() {
                options.max_height = None;
            }
            if other.layout.is_some() {
                options.reverse = None;
            }
        }
        if let Some(options) = &other.layout_options {
            if let Some(current) = &self.layout_options {
                let mut merged = **current;
                merged.merge(**options);
                if merged != **current {
                    self.layout_options = Some(std::sync::Arc::new(merged));
                }
            } else {
                self.layout_options = Some(options.clone());
            }
        }
        if let Some(rare) = &other.rare {
            match &mut self.rare {
                Some(own) => std::sync::Arc::make_mut(own).merge(rare),
                None => self.rare = Some(rare.clone()),
            }
        }
        if other.paint_background.is_some() {
            self.paint_background = other.paint_background.clone();
        }
        if other.corners.is_some() {
            self.corners = other.corners;
        }
        if other.border_edges.is_some() {
            self.border_edges = other.border_edges;
        }
        if other.object_fit.is_some() {
            self.object_fit = other.object_fit;
        }
        if other.absolute.is_some() {
            self.absolute = other.absolute;
        }
        if other.layout.is_some() {
            self.layout = other.layout;
        }
        if other.width.is_some() || other.width_percent.is_some() {
            self.width = other.width;
            self.width_percent = other.width_percent;
        }
        if other.height.is_some() || other.height_percent.is_some() {
            self.height = other.height;
            self.height_percent = other.height_percent;
        }
        if other.gap.is_some() {
            self.gap = other.gap;
        }
        if other.clip.is_some() {
            self.clip = other.clip;
        }
        if other.flex_grow.is_some() {
            self.flex_grow = other.flex_grow;
        }
        if other.flex_shrink.is_some() {
            self.flex_shrink = other.flex_shrink;
        }
        if other.align.is_some() {
            self.align = other.align;
        }
        if other.justify.is_some() {
            self.justify = other.justify;
        }
        if other.text_wrap.is_some() {
            self.text_wrap = other.text_wrap;
        }
        if other.cursor.is_some() {
            self.cursor = other.cursor;
        }
        if other.background.is_some() {
            self.background = other.background;
        }
        if other.text_color.is_some() {
            self.text_color = other.text_color;
        }
        if other.text_align.is_some() {
            self.text_align = other.text_align;
        }
        if other.font_family.is_some() {
            self.font_family = other.font_family.clone();
        }
        if other.font_weight.is_some() {
            self.font_weight = other.font_weight;
        }
        if other.italic.is_some() {
            self.italic = other.italic;
        }
        if other.text_size.is_some() {
            self.text_size = other.text_size;
        }
        if other.line_height.is_some() {
            self.line_height = other.line_height;
        }
        if other.radius.is_some() {
            self.radius = other.radius;
        }
        if other.border_width.is_some() {
            self.border_width = other.border_width;
        }
        if other.border_color.is_some() {
            self.border_color = other.border_color;
        }
        if other.opacity.is_some() {
            self.opacity = other.opacity;
        }
        if other.blur.is_some() {
            self.blur = other.blur;
        }
        if other.edge_fade.is_some() {
            self.edge_fade = other.edge_fade;
        }
        if other.transform.is_some() {
            self.transform = other.transform;
        }
        if other.isolated.is_some() {
            self.isolated = other.isolated;
        }
        if let Some(value) = other.padding {
            if self.padding.is_some() && self.padding_sides == 0 {
                self.padding_sides = 15;
            }
            let mask = if other.padding_sides == 0 {
                15
            } else {
                other.padding_sides
            };
            merge_insets(
                self.padding.get_or_insert_with(Insets::default),
                value,
                mask,
            );
            self.padding_sides |= mask;
        }
        if let Some(value) = other.margin {
            if self.margin.is_some() && self.margin_sides == 0 {
                self.margin_sides = 15;
            }
            let mask = if other.margin_sides == 0 {
                15
            } else {
                other.margin_sides
            };
            merge_insets(self.margin.get_or_insert_with(Insets::default), value, mask);
            self.margin_sides |= mask;
        }
    }
}
fn merge_insets(target: &mut Insets, source: Insets, mask: u8) {
    if mask & 1 != 0 {
        target.top = source.top;
    }
    if mask & 2 != 0 {
        target.right = source.right;
    }
    if mask & 4 != 0 {
        target.bottom = source.bottom;
    }
    if mask & 8 != 0 {
        target.left = source.left;
    }
}
/// Shared fluent styling API for views and reusable style patches.
pub trait Styled: Sized {
    fn styles_mut(&mut self) -> &mut Styles;
    /// Restore flex display after `hidden` or `grid`, preserving its direction.
    fn flex(mut self) -> Self {
        self.styles_mut().layout_options_mut().display = Some(crate::layout::Display::Flex);
        self
    }
    /// Merge advanced layout options without changing unrelated style properties.
    fn layout_options(mut self, options: crate::layout::LayoutOptions) -> Self {
        self.styles_mut().layout_options_mut().merge(options);
        self
    }
    fn grid(mut self) -> Self {
        self.styles_mut().layout_options_mut().display = Some(crate::layout::Display::Grid);
        self
    }
    fn hidden(mut self) -> Self {
        self.styles_mut().layout_options_mut().display = Some(crate::layout::Display::None);
        self
    }
    fn visible(mut self) -> Self {
        self.styles_mut().layout_options_mut().visible = Some(true);
        self
    }
    fn invisible(mut self) -> Self {
        self.styles_mut().layout_options_mut().visible = Some(false);
        self
    }
    fn flex_wrap(mut self) -> Self {
        self.styles_mut().layout_options_mut().wrap = Some(crate::layout::FlexWrap::Wrap);
        self
    }
    fn flex_wrap_reverse(mut self) -> Self {
        self.styles_mut().layout_options_mut().wrap = Some(crate::layout::FlexWrap::Reverse);
        self
    }
    fn flex_nowrap(mut self) -> Self {
        self.styles_mut().layout_options_mut().wrap = Some(crate::layout::FlexWrap::NoWrap);
        self
    }
    fn flex_row_reverse(mut self) -> Self {
        self.styles_mut().layout = Some(Layout::Row);
        self.styles_mut().layout_options_mut().reverse = Some(true);
        self
    }
    fn flex_col_reverse(mut self) -> Self {
        self.styles_mut().layout = Some(Layout::Column);
        self.styles_mut().layout_options_mut().reverse = Some(true);
        self
    }
    fn flex_basis(mut self, value: impl Into<crate::layout::Length>) -> Self {
        self.styles_mut().layout_options_mut().basis = Some(value.into());
        self
    }
    fn align_self(mut self, value: Align) -> Self {
        self.styles_mut().layout_options_mut().align_self = Some(value);
        self
    }
    fn align_content(mut self, value: crate::layout::ContentAlign) -> Self {
        self.styles_mut().layout_options_mut().align_content = Some(value);
        self
    }
    fn gap_x(mut self, value: impl Into<crate::layout::Length>) -> Self {
        self.styles_mut().layout_options_mut().gap_x = Some(value.into());
        self
    }
    fn gap_y(mut self, value: impl Into<crate::layout::Length>) -> Self {
        self.styles_mut().layout_options_mut().gap_y = Some(value.into());
        self
    }
    fn aspect_ratio(mut self, value: f32) -> Self {
        self.styles_mut().layout_options_mut().aspect_ratio = Some(value);
        self
    }
    fn grid_cols(mut self, value: u16) -> Self {
        self.styles_mut().layout_options_mut().columns = Some(value.max(1));
        self
    }
    fn grid_rows(mut self, value: u16) -> Self {
        self.styles_mut().layout_options_mut().rows = Some(value.max(1));
        self
    }
    fn col_start(mut self, value: i16) -> Self {
        self.styles_mut().layout_options_mut().column_start = Some(value);
        self
    }
    fn row_start(mut self, value: i16) -> Self {
        self.styles_mut().layout_options_mut().row_start = Some(value);
        self
    }
    fn col_span(mut self, value: u16) -> Self {
        self.styles_mut().layout_options_mut().column_span = Some(value.max(1));
        self
    }
    fn row_span(mut self, value: u16) -> Self {
        self.styles_mut().layout_options_mut().row_span = Some(value.max(1));
        self
    }
    fn col_span_full(mut self) -> Self {
        self.styles_mut().layout_options_mut().column_full = Some(true);
        self
    }
    fn row_span_full(mut self) -> Self {
        self.styles_mut().layout_options_mut().row_full = Some(true);
        self
    }
    fn min_w_percent(mut self, percent: f32) -> Self {
        self.styles_mut().layout_options_mut().min_width =
            Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn max_w_percent(mut self, percent: f32) -> Self {
        self.styles_mut().layout_options_mut().max_width =
            Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn min_h_percent(mut self, percent: f32) -> Self {
        self.styles_mut().layout_options_mut().min_height =
            Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn max_h_percent(mut self, percent: f32) -> Self {
        self.styles_mut().layout_options_mut().max_height =
            Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn top(mut self, value: impl Into<crate::layout::Length>) -> Self {
        self.styles_mut().layout_options_mut().inset.top = Some(value.into());
        self
    }
    fn right(mut self, value: impl Into<crate::layout::Length>) -> Self {
        self.styles_mut().layout_options_mut().inset.right = Some(value.into());
        self
    }
    fn bottom(mut self, value: impl Into<crate::layout::Length>) -> Self {
        self.styles_mut().layout_options_mut().inset.bottom = Some(value.into());
        self
    }
    fn left(mut self, value: impl Into<crate::layout::Length>) -> Self {
        self.styles_mut().layout_options_mut().inset.left = Some(value.into());
        self
    }
    fn p_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.padding.top = Some(crate::layout::Length::Percent(percent / 100.));
        options.padding.right = Some(crate::layout::Length::Percent(percent / 100.));
        options.padding.bottom = Some(crate::layout::Length::Percent(percent / 100.));
        options.padding.left = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn px_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.padding.right = Some(crate::layout::Length::Percent(percent / 100.));
        options.padding.left = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn py_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.padding.top = Some(crate::layout::Length::Percent(percent / 100.));
        options.padding.bottom = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn pt_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.padding.top = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn pr_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.padding.right = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn pb_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.padding.bottom = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn pl_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.padding.left = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn m_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.top = Some(crate::layout::Length::Percent(percent / 100.));
        options.margin.right = Some(crate::layout::Length::Percent(percent / 100.));
        options.margin.bottom = Some(crate::layout::Length::Percent(percent / 100.));
        options.margin.left = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn m_auto(mut self) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.top = Some(crate::layout::Length::Auto);
        options.margin.right = Some(crate::layout::Length::Auto);
        options.margin.bottom = Some(crate::layout::Length::Auto);
        options.margin.left = Some(crate::layout::Length::Auto);
        self
    }
    fn mx_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.right = Some(crate::layout::Length::Percent(percent / 100.));
        options.margin.left = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn mx_auto(mut self) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.right = Some(crate::layout::Length::Auto);
        options.margin.left = Some(crate::layout::Length::Auto);
        self
    }
    fn my_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.top = Some(crate::layout::Length::Percent(percent / 100.));
        options.margin.bottom = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn my_auto(mut self) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.top = Some(crate::layout::Length::Auto);
        options.margin.bottom = Some(crate::layout::Length::Auto);
        self
    }
    fn mt_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.top = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn mt_auto(mut self) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.top = Some(crate::layout::Length::Auto);
        self
    }
    fn mr_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.right = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn mr_auto(mut self) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.right = Some(crate::layout::Length::Auto);
        self
    }
    fn mb_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.bottom = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn mb_auto(mut self) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.bottom = Some(crate::layout::Length::Auto);
        self
    }
    fn ml_percent(mut self, percent: f32) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.left = Some(crate::layout::Length::Percent(percent / 100.));
        self
    }
    fn ml_auto(mut self) -> Self {
        let options = self.styles_mut().layout_options_mut();
        options.margin.left = Some(crate::layout::Length::Auto);
        self
    }
    /// Position at the parent content origin plus margins, outside normal flow.
    /// Absolute children do not contribute to parent intrinsic size or flex space.
    fn absolute(mut self) -> Self {
        self.styles_mut().absolute = Some(true);
        self
    }
    /// Restore normal row, column, or overlay layout participation.
    fn relative(mut self) -> Self {
        self.styles_mut().absolute = Some(false);
        self
    }
    /// Apply a reusable transformation while building a view or style patch.
    fn apply(self, transform: impl FnOnce(Self) -> Self) -> Self {
        transform(self)
    }
    /// Apply a transformation only when `condition` is true.
    ///
    /// This decision is made immediately, not reactively. Use a view's
    /// `reactive_style` method for styles that follow signal changes.
    fn when(self, condition: bool, transform: impl FnOnce(Self) -> Self) -> Self {
        if condition { transform(self) } else { self }
    }
    /// Apply a transformation with the contained value, if present.
    ///
    /// Like `when`, this runs while building the description, without creating
    /// a subscription. The closure is not called for `None`.
    fn when_some<T>(self, value: Option<T>, transform: impl FnOnce(Self, T) -> Self) -> Self {
        match value {
            Some(value) => transform(self, value),
            None => self,
        }
    }
    fn style(mut self, style: Styles) -> Self {
        self.styles_mut().merge(&style);
        self
    }
    /// Set centered image fitting within its padded content box. Not inherited.
    fn object_fit(mut self, fit: ObjectFit) -> Self {
        self.styles_mut().object_fit = Some(fit);
        self
    }
    fn object_contain(self) -> Self {
        self.object_fit(ObjectFit::Contain)
    }
    fn object_cover(self) -> Self {
        self.object_fit(ObjectFit::Cover)
    }
    fn w(mut self, value: f32) -> Self {
        self.styles_mut().width = Some(value);
        self.styles_mut().width_percent = None;
        self
    }
    fn h(mut self, value: f32) -> Self {
        self.styles_mut().height = Some(value);
        self.styles_mut().height_percent = None;
        self
    }
    /// Width as a percentage of the parent's definite content width; 100 fills it.
    /// Indefinite parent widths use intrinsic sizing. Flex allocation and min/max
    /// constraints still apply. Negative and non-finite values normalize to zero.
    fn w_percent(mut self, percent: f32) -> Self {
        self.styles_mut().width = None;
        self.styles_mut().width_percent = Some(if percent.is_finite() {
            percent.max(0.) / 100.
        } else {
            0.
        });
        self
    }
    /// Height as a percentage of the parent's definite content height.
    /// Uses the same normalization and intrinsic fallback as `w_percent`.
    fn h_percent(mut self, percent: f32) -> Self {
        self.styles_mut().height = None;
        self.styles_mut().height_percent = Some(if percent.is_finite() {
            percent.max(0.) / 100.
        } else {
            0.
        });
        self
    }
    /// Fill the parent's definite content width, subject to flex and min/max rules.
    fn w_full(self) -> Self {
        self.w_percent(100.)
    }
    /// Fill the parent's definite content height, subject to flex and min/max rules.
    fn h_full(self) -> Self {
        self.h_percent(100.)
    }
    fn min_w(mut self, value: f32) -> Self {
        if let Some(options) = &mut self.styles_mut().layout_options {
            std::sync::Arc::make_mut(options).min_width = None;
        }
        self.styles_mut().rare_mut().min_width = Some(value);
        self
    }
    fn max_w(mut self, value: f32) -> Self {
        if let Some(options) = &mut self.styles_mut().layout_options {
            std::sync::Arc::make_mut(options).max_width = None;
        }
        self.styles_mut().rare_mut().max_width = Some(value);
        self
    }
    fn min_h(mut self, value: f32) -> Self {
        if let Some(options) = &mut self.styles_mut().layout_options {
            std::sync::Arc::make_mut(options).min_height = None;
        }
        self.styles_mut().rare_mut().min_height = Some(value);
        self
    }
    fn max_h(mut self, value: f32) -> Self {
        if let Some(options) = &mut self.styles_mut().layout_options {
            std::sync::Arc::make_mut(options).max_height = None;
        }
        self.styles_mut().rare_mut().max_height = Some(value);
        self
    }
    fn gap(mut self, value: f32) -> Self {
        if let Some(options) = &mut self.styles_mut().layout_options {
            let options = std::sync::Arc::make_mut(options);
            options.gap_x = None;
            options.gap_y = None;
        }
        self.styles_mut().gap = Some(value);
        self
    }
    fn flex_grow(mut self, value: f32) -> Self {
        self.styles_mut().flex_grow = Some(value);
        self
    }
    fn flex_shrink(mut self, value: f32) -> Self {
        self.styles_mut().flex_shrink = Some(value);
        self
    }
    fn bg_fill(mut self, brush: crate::canvas::Brush) -> Self {
        self.styles_mut().paint_background =
            Some(Some(crate::decoration::Background::Brush(brush)));
        self
    }
    fn bg_gradient(
        mut self,
        angle: f32,
        stops: impl Into<std::sync::Arc<[crate::canvas::GradientStop]>>,
    ) -> Self {
        self.styles_mut().paint_background = Some(Some(crate::decoration::Background::Linear {
            angle,
            stops: stops.into(),
        }));
        self
    }
    fn rounded_corners(mut self, corners: crate::decoration::Corners) -> Self {
        self.styles_mut().corners = Some(Some(corners));
        self
    }
    fn border_edges(mut self, widths: Insets) -> Self {
        self.styles_mut().border_edges = Some(Some(widths));
        self
    }
    fn border_style(mut self, style: crate::decoration::BorderStyle) -> Self {
        self.styles_mut().rare_mut().border_style = Some(style);
        self
    }
    fn shadows(mut self, shadows: impl Into<std::sync::Arc<[BoxShadow]>>) -> Self {
        self.styles_mut().rare_mut().shadows = Some(Some(shadows.into()));
        self
    }
    fn cursor(mut self, cursor: crate::cursor::Cursor) -> Self {
        self.styles_mut().cursor = Some(cursor);
        self
    }
    fn bg(mut self, value: Color) -> Self {
        self.styles_mut().background = Some(value);
        self.styles_mut().paint_background = Some(None);
        self
    }
    fn text_color(mut self, value: Color) -> Self {
        self.styles_mut().text_color = Some(value);
        self
    }
    /// Ordered explicit font families tried before platform fallback.
    fn font_fallbacks(
        mut self,
        families: impl Into<std::sync::Arc<[crate::text_layout::FontFamily]>>,
    ) -> Self {
        self.styles_mut().rare_mut().font_fallbacks = Some(families.into());
        self
    }
    fn text_overflow(mut self, overflow: crate::text_layout::TextOverflow) -> Self {
        self.styles_mut().rare_mut().text_overflow = Some(overflow);
        self
    }
    /// Clamp displayed visual lines; zero removes an inherited clamp.
    fn line_clamp(mut self, lines: u32) -> Self {
        self.styles_mut().rare_mut().line_clamp = Some(std::num::NonZeroU32::new(lines));
        self
    }
    fn truncate(self) -> Self {
        self.line_clamp(1)
            .text_overflow(crate::text_layout::TextOverflow::Ellipsis)
    }
    fn text_align(mut self, alignment: crate::text_layout::TextAlign) -> Self {
        self.styles_mut().text_align = Some(alignment);
        self
    }
    /// Override inherited OpenType features; an empty set restores font defaults.
    fn font_features(mut self, features: crate::text_layout::FontFeatures) -> Self {
        self.styles_mut().rare_mut().font_features = Some(features);
        self
    }
    fn font_family(mut self, family: impl Into<crate::text_layout::FontFamily>) -> Self {
        self.styles_mut().font_family = Some(family.into());
        self
    }
    fn font_weight(mut self, weight: u16) -> Self {
        self.styles_mut().font_weight = Some(weight.clamp(1, 1000));
        self
    }
    fn font_bold(self) -> Self {
        self.font_weight(700)
    }
    fn italic(mut self, italic: bool) -> Self {
        self.styles_mut().italic = Some(italic);
        self
    }
    fn text_size(mut self, value: f32) -> Self {
        self.styles_mut().text_size = Some(value);
        self
    }
    /// Set inherited line spacing in logical pixels. Invalid/non-positive values
    /// use normal spacing; positive values smaller than the font size are allowed.
    fn line_height(mut self, pixels: f32) -> Self {
        self.styles_mut().line_height = Some(crate::text_layout::LineHeight::px(pixels));
        self
    }
    /// Reset inherited line spacing to the normal metric for each text size.
    fn line_height_normal(mut self) -> Self {
        self.styles_mut().line_height = Some(crate::text_layout::LineHeight::NORMAL);
        self
    }
    /// Inherited extra glyph advance in logical pixels. Zero resets inherited tracking.
    /// Non-finite values normalize to zero; finite values clamp to ±1,000,000 pixels.
    fn letter_spacing(mut self, pixels: f32) -> Self {
        self.styles_mut().rare_mut().letter_spacing = Some(crate::text_layout::LetterSpacing::px(pixels));
        self
    }

    fn rounded(mut self, value: f32) -> Self {
        self.styles_mut().radius = Some(value);
        self.styles_mut().corners = Some(None);
        self
    }
    fn border(mut self, value: f32) -> Self {
        self.styles_mut().border_width = Some(value);
        self.styles_mut().border_edges = Some(None);
        self
    }
    fn border_color(mut self, value: Color) -> Self {
        self.styles_mut().border_color = Some(value);
        self
    }
    fn opacity(mut self, value: f32) -> Self {
        self.styles_mut().opacity = Some(value);
        self
    }
    fn blur(mut self, value: f32) -> Self {
        self.styles_mut().blur = Some(value);
        self
    }
    fn edge_fade(mut self, value: f32) -> Self {
        self.styles_mut().edge_fade = Some(value);
        self
    }
    fn isolated(mut self, value: bool) -> Self {
        self.styles_mut().isolated = Some(value);
        self
    }
    fn text_wrap(mut self, value: bool) -> Self {
        self.styles_mut().text_wrap = Some(value);
        self
    }
    fn size(self, width: f32, height: f32) -> Self {
        self.w(width).h(height)
    }
    fn p(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.padding.is_some() && style.padding_sides == 0 {
            style.padding_sides = 15;
        }
        merge_insets(
            style.padding.get_or_insert_with(Insets::default),
            Insets::all(value),
            15,
        );
        style.padding_sides |= 15;
        style.clear_layout_spacing(false, 15);
        self
    }
    fn px(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.padding.is_some() && style.padding_sides == 0 {
            style.padding_sides = 15;
        }
        merge_insets(
            style.padding.get_or_insert_with(Insets::default),
            Insets::all(value),
            10,
        );
        style.padding_sides |= 10;
        style.clear_layout_spacing(false, 10);
        self
    }
    fn py(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.padding.is_some() && style.padding_sides == 0 {
            style.padding_sides = 15;
        }
        merge_insets(
            style.padding.get_or_insert_with(Insets::default),
            Insets::all(value),
            5,
        );
        style.padding_sides |= 5;
        style.clear_layout_spacing(false, 5);
        self
    }
    fn pt(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.padding.is_some() && style.padding_sides == 0 {
            style.padding_sides = 15;
        }
        merge_insets(
            style.padding.get_or_insert_with(Insets::default),
            Insets::all(value),
            1,
        );
        style.padding_sides |= 1;
        style.clear_layout_spacing(false, 1);
        self
    }
    fn pr(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.padding.is_some() && style.padding_sides == 0 {
            style.padding_sides = 15;
        }
        merge_insets(
            style.padding.get_or_insert_with(Insets::default),
            Insets::all(value),
            2,
        );
        style.padding_sides |= 2;
        style.clear_layout_spacing(false, 2);
        self
    }
    fn pb(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.padding.is_some() && style.padding_sides == 0 {
            style.padding_sides = 15;
        }
        merge_insets(
            style.padding.get_or_insert_with(Insets::default),
            Insets::all(value),
            4,
        );
        style.padding_sides |= 4;
        style.clear_layout_spacing(false, 4);
        self
    }
    fn pl(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.padding.is_some() && style.padding_sides == 0 {
            style.padding_sides = 15;
        }
        merge_insets(
            style.padding.get_or_insert_with(Insets::default),
            Insets::all(value),
            8,
        );
        style.padding_sides |= 8;
        style.clear_layout_spacing(false, 8);
        self
    }
    fn m(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.margin.is_some() && style.margin_sides == 0 {
            style.margin_sides = 15;
        }
        merge_insets(
            style.margin.get_or_insert_with(Insets::default),
            Insets::all(value),
            15,
        );
        style.margin_sides |= 15;
        style.clear_layout_spacing(true, 15);
        self
    }
    fn mx(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.margin.is_some() && style.margin_sides == 0 {
            style.margin_sides = 15;
        }
        merge_insets(
            style.margin.get_or_insert_with(Insets::default),
            Insets::all(value),
            10,
        );
        style.margin_sides |= 10;
        style.clear_layout_spacing(true, 10);
        self
    }
    fn my(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.margin.is_some() && style.margin_sides == 0 {
            style.margin_sides = 15;
        }
        merge_insets(
            style.margin.get_or_insert_with(Insets::default),
            Insets::all(value),
            5,
        );
        style.margin_sides |= 5;
        style.clear_layout_spacing(true, 5);
        self
    }
    fn mt(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.margin.is_some() && style.margin_sides == 0 {
            style.margin_sides = 15;
        }
        merge_insets(
            style.margin.get_or_insert_with(Insets::default),
            Insets::all(value),
            1,
        );
        style.margin_sides |= 1;
        style.clear_layout_spacing(true, 1);
        self
    }
    fn mr(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.margin.is_some() && style.margin_sides == 0 {
            style.margin_sides = 15;
        }
        merge_insets(
            style.margin.get_or_insert_with(Insets::default),
            Insets::all(value),
            2,
        );
        style.margin_sides |= 2;
        style.clear_layout_spacing(true, 2);
        self
    }
    fn mb(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.margin.is_some() && style.margin_sides == 0 {
            style.margin_sides = 15;
        }
        merge_insets(
            style.margin.get_or_insert_with(Insets::default),
            Insets::all(value),
            4,
        );
        style.margin_sides |= 4;
        style.clear_layout_spacing(true, 4);
        self
    }
    fn ml(mut self, value: f32) -> Self {
        let style = self.styles_mut();
        if style.margin.is_some() && style.margin_sides == 0 {
            style.margin_sides = 15;
        }
        merge_insets(
            style.margin.get_or_insert_with(Insets::default),
            Insets::all(value),
            8,
        );
        style.margin_sides |= 8;
        style.clear_layout_spacing(true, 8);
        self
    }
    fn flex_row(mut self) -> Self {
        if let Some(options) = &mut self.styles_mut().layout_options {
            std::sync::Arc::make_mut(options).reverse = None;
        }
        self.styles_mut().layout = Some(Layout::Row);
        self
    }
    fn flex_col(mut self) -> Self {
        if let Some(options) = &mut self.styles_mut().layout_options {
            std::sync::Arc::make_mut(options).reverse = None;
        }
        self.styles_mut().layout = Some(Layout::Column);
        self
    }
    fn overlay(mut self) -> Self {
        self.styles_mut().layout = Some(Layout::Overlay);
        self
    }
    fn items_start(mut self) -> Self {
        self.styles_mut().align = Some(Align::Start);
        self
    }
    fn items_center(mut self) -> Self {
        self.styles_mut().align = Some(Align::Center);
        self
    }
    fn items_baseline(mut self) -> Self {
        self.styles_mut().align = Some(Align::Baseline);
        self
    }
    fn items_end(mut self) -> Self {
        self.styles_mut().align = Some(Align::End);
        self
    }
    fn items_stretch(mut self) -> Self {
        self.styles_mut().align = Some(Align::Stretch);
        self
    }
    fn justify_start(mut self) -> Self {
        self.styles_mut().justify = Some(Justify::Start);
        self
    }
    fn justify_center(mut self) -> Self {
        self.styles_mut().justify = Some(Justify::Center);
        self
    }
    fn justify_end(mut self) -> Self {
        self.styles_mut().justify = Some(Justify::End);
        self
    }
    fn justify_between(mut self) -> Self {
        self.styles_mut().justify = Some(Justify::SpaceBetween);
        self
    }
    fn justify_evenly(mut self) -> Self {
        self.styles_mut().justify = Some(Justify::SpaceEvenly);
        self
    }
    fn justify_around(mut self) -> Self {
        self.styles_mut().justify = Some(Justify::SpaceAround);
        self
    }
    /// Fade content out over `top` and `bottom` bands at the edges of this
    /// node's clip (with `overflow_hidden` or a scroll view), per pixel.
    fn fade_edges(mut self, top: f32, bottom: f32) -> Self {
        self.styles_mut().rare_mut().fade_edges = Some([top, bottom]);
        self
    }
    fn overflow_hidden(mut self) -> Self {
        self.styles_mut().clip = Some(true);
        if let Some(options) = &mut self.styles_mut().layout_options {
            let options = std::sync::Arc::make_mut(options);
            options.clip_x = None;
            options.clip_y = None;
        }
        self
    }
    fn overflow_visible(mut self) -> Self {
        self.styles_mut().clip = Some(false);
        if let Some(options) = &mut self.styles_mut().layout_options {
            let options = std::sync::Arc::make_mut(options);
            options.clip_x = None;
            options.clip_y = None;
        }
        self
    }
    fn overflow_x_hidden(mut self) -> Self {
        self.styles_mut().layout_options_mut().clip_x = Some(true);
        self
    }
    fn overflow_y_hidden(mut self) -> Self {
        self.styles_mut().layout_options_mut().clip_y = Some(true);
        self
    }
    fn overflow_x_visible(mut self) -> Self {
        self.styles_mut().layout_options_mut().clip_x = Some(false);
        self
    }
    fn overflow_y_visible(mut self) -> Self {
        self.styles_mut().layout_options_mut().clip_y = Some(false);
        self
    }
    fn border_1(self) -> Self {
        self.border(1.)
    }
    fn grow(self) -> Self {
        self.flex_grow(1.)
    }
    fn shrink_0(self) -> Self {
        self.flex_shrink(0.)
    }
    fn shadow(mut self, value: BoxShadow) -> Self {
        self.styles_mut().rare_mut().shadow = Some(Some(value));
        self.styles_mut().rare_mut().shadows = Some(None);
        self
    }
    fn shadow_none(mut self) -> Self {
        self.styles_mut().rare_mut().shadow = Some(None);
        self.styles_mut().rare_mut().shadows = Some(None);
        self
    }
    fn translate(mut self, x: f32, y: f32) -> Self {
        self.styles_mut().transform = Some(Transform { x, y });
        self
    }
}
impl Styled for Styles {
    fn styles_mut(&mut self) -> &mut Styles {
        self
    }
}
/// Opaque color from `0xRRGGBB`.
pub const fn rgb(value: u32) -> Color {
    Color((value >> 16) as u8, (value >> 8) as u8, value as u8, 255)
}
/// Color from `0xRRGGBBAA`, with alpha in the least significant byte.
pub const fn rgba(value: u32) -> Color {
    Color(
        (value >> 24) as u8,
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentage_patches_normalize_invalid_values_and_replace_pixel_axes() {
        let patch = Styles::new().size(80., 30.).w_percent(50.).h_full();
        assert_eq!(patch.width, None);
        assert_eq!(patch.height, None);
        assert_eq!(patch.width_percent, Some(0.5));
        assert_eq!(patch.height_percent, Some(1.));
        let pixels = patch.style(Styles::new().w(20.));
        assert_eq!(pixels.width, Some(20.));
        assert_eq!(pixels.width_percent, None);
        assert_eq!(pixels.height_percent, Some(1.));
        for invalid in [-1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                Styles::new().w_percent(invalid),
                Styles::new().w_percent(0.)
            );
        }
        assert_eq!(Styles::new().w_percent(150.).width_percent, Some(1.5));
    }
    #[test]
    fn conditional_transformations_preserve_sparse_patch_edges() {
        fn horizontal<S: Styled>(style: S) -> S {
            style.px(12.)
        }

        let patch = Styles::new()
            .apply(horizontal)
            .when(false, |_| panic!("false branch must not run"))
            .when_some(None::<f32>, |_, _| panic!("missing value must not run"))
            .when(true, |style| style.pl(14.))
            .when_some(Some(String::from("owned")), |style, label| {
                assert_eq!(label, "owned");
                style.pr(16.)
            });
        assert_eq!(patch.padding_sides, 2 | 8);
        let result = Styles::new().p(8.).pt(3.).style(patch);
        assert_eq!(
            result.padding,
            Some(Insets {
                top: 3.,
                right: 16.,
                bottom: 8.,
                left: 14.,
            })
        );
    }
    #[test]
    fn patches_only_override_specified_properties() {
        let base = Styles::new()
            .size(120., 40.)
            .gap(8.)
            .bg(rgb(0x102030))
            .opacity(0.5);
        let result = base.clone().style(Styles::new().w(200.).border_1());
        assert_eq!(result.width, Some(200.));
        assert_eq!(result.height, base.height);
        assert_eq!(result.gap, base.gap);
        assert_eq!(result.background, base.background);
        assert_eq!(result.opacity, base.opacity);
        assert_eq!(result.border_width, Some(1.));
        assert_eq!(result.clone().style(Styles::new()), result);
    }
    #[test]
    fn side_spacing_obeys_chain_and_merge_precedence() {
        let style = Styles::new().p(8.).px(12.).pt(3.).m(4.).mb(9.);
        assert_eq!(
            style.padding,
            Some(Insets {
                top: 3.,
                right: 12.,
                bottom: 8.,
                left: 12.
            })
        );
        assert_eq!(
            style.margin,
            Some(Insets {
                top: 4.,
                right: 4.,
                bottom: 9.,
                left: 4.
            })
        );
        let merged = style.style(Styles::new().py(7.).ml(2.));
        assert_eq!(
            merged.padding,
            Some(Insets {
                top: 7.,
                right: 12.,
                bottom: 7.,
                left: 12.
            })
        );
        assert_eq!(
            merged.margin,
            Some(Insets {
                top: 4.,
                right: 4.,
                bottom: 9.,
                left: 2.
            })
        );
        assert_eq!(merged.p(1.).padding, Some(Insets::all(1.)));
    }
    #[test]
    fn effect_reset_and_color_order_are_explicit() {
        let shadow = BoxShadow {
            color: rgba(0x10203040),
            offset: Transform { x: 1., y: 2. },
            blur_radius: 4.,
            spread: 0.,
        };
        let style = Styles::new()
            .shadow(shadow)
            .overflow_hidden()
            .isolated(true)
            .style(
                Styles::new()
                    .shadow_none()
                    .overflow_visible()
                    .isolated(false),
            );
        assert_eq!(style.rare().shadow, Some(None));
        assert_eq!(style.clip, Some(false));
        assert_eq!(style.isolated, Some(false));
        assert_eq!(rgb(0x102030), Color(16, 32, 48, 255));
        assert_eq!(rgba(0x10203040), Color(16, 32, 48, 64));
    }
}
