//! LaTeX math in markdown: `$…$` inline and `$$…$$` display formulas,
//! typeset by RaTeX and drawn as images in the surrounding text's color.
//!
//! The markdown parser always recognizes math, but without a plugin a `$$`
//! block shows as a code block of raw LaTeX and `$…$` as literal text. The
//! plugins here claim those nodes when the formula parses, and leave the rest
//! to that fallback, so prose like "$5 and $10" stays prose.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::component::text::{
    InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin, markdown_ast::Node,
};
use gpui_kit::*;
use image::{Frame, ImageFormat};
use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_render::{RenderOptions, render_to_png};
use ratex_types::{Color, MathStyle};

/// Logical pixels of transparent margin RaTeX leaves around a formula: enough
/// to keep antialiased edges, small enough to sit tight in a line of text.
const PAD: f32 = 1.0;
/// Display math reads a step larger than the body, as in typeset documents.
const DISPLAY_SCALE: f32 = 1.15;
/// Rendered formulas kept before the cache starts over. Each is one small
/// image; the bound only stops a long session from growing without limit.
const CACHE_LIMIT: usize = 512;

/// Markdown extensions with both math plugins. Built once and cloned, so every
/// view shares one parser revision and keeps its parsed document across frames.
pub fn extensions() -> MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS
        .get_or_init(|| {
            MarkdownExtensions::default()
                .plugin(InlineMath)
                .plugin(BlockMath)
        })
        .clone()
}

/// The LaTeX of one formula, carried from parse to render.
struct Formula {
    latex: String,
    /// Display style: a `$$` block, or a `$$…$$` span inside a line, which
    /// markdown parses as inline math but is written as a display formula.
    display: bool,
}

struct InlineMath;

impl MarkdownPlugin for InlineMath {
    fn name(&self) -> &str {
        "math-inline"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::InlineMath(math) = node else {
            return None;
        };
        let position = node.position()?;
        let range = position.start.offset..position.end.offset;
        let display = inline_formula(cx.source(), range.clone(), &math.value)?;
        let source = cx.source().get(range)?;
        Some(formula_node("math-inline", &math.value, source, display))
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, _: &mut App) -> impl IntoElement {
        node.as_text().to_string()
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        _: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        let style = context.text_style();
        let scale = if formula.display { DISPLAY_SCALE } else { 1. };
        let Some(raster) = raster(
            &formula.latex,
            formula.display,
            f32::from(context.font_size()) * scale,
            window.scale_factor(),
            style.color,
        ) else {
            return Some(InlineElement::new(node.as_text().to_string()));
        };
        Some(InlineElement::new(image(&raster)).with_baseline(px(raster.baseline)))
    }
}

struct BlockMath;

impl MarkdownPlugin for BlockMath {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "math-block"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Math(math) = node else {
            return None;
        };
        let source = cx.node_source(node)?;
        if !claims_block(source, &math.value) {
            return None;
        }
        Some(formula_node("math-block", &math.value, source, true))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, _: &mut App) -> impl IntoElement {
        let text = node.as_text().to_string();
        let Some(formula) = node.data::<Formula>() else {
            return div().child(text).into_any_element();
        };
        let style = window.text_style();
        let font_size = f32::from(style.font_size.to_pixels(window.rem_size())) * DISPLAY_SCALE;
        let Some(raster) = raster(
            &formula.latex,
            true,
            font_size,
            window.scale_factor(),
            style.color,
        ) else {
            return div().child(text).into_any_element();
        };
        // Flush left like the rest of the transcript; a formula wider than the
        // column scrolls instead of overflowing it. Keyed by where it sits in
        // the document, so two identical formulas keep their own scroll.
        let at = node.source_range().map_or(0, |range| range.start);
        div()
            .id(("math", at))
            .w_full()
            .overflow_x_scroll()
            .child(image(&raster))
            .into_any_element()
    }
}

/// A custom node whose text and markdown are the formula's source, delimiters
/// included, so copying a selection gives back the LaTeX that was written.
fn formula_node(name: &'static str, latex: &str, source: &str, display: bool) -> MarkdownNode {
    MarkdownNode::new(
        name,
        Formula {
            latex: latex.to_string(),
            display,
        },
    )
    .text(source.to_string())
    .markdown(source.to_string())
    .accessibility_label(latex.to_string())
}

/// Whether the inline math at `range` of `doc`, holding `value`, is claimed as
/// a formula, and if so whether it is in display style (written `$$…$$`).
fn inline_formula(doc: &str, range: Range<usize>, value: &str) -> Option<bool> {
    let source = doc.get(range.clone())?;
    let next = doc.get(range.end..).and_then(|rest| rest.chars().next());
    (is_inline_formula(source, value, next) && typesets(value)).then_some(source.starts_with("$$"))
}

/// Whether a `$$` block is claimed as a formula. While a reply streams, an
/// unclosed `$$` runs to the end of the text and grows every frame: each
/// prefix would typeset and rasterize anew, so it stays a code block until
/// its closing fence arrives.
fn claims_block(source: &str, value: &str) -> bool {
    is_closed_block(source) && !value.trim().is_empty() && typesets(value)
}

/// Whether an inline `$…$` span is a formula rather than prose with dollar
/// signs. Follows Pandoc's rule for single dollars: the opening `$` is followed
/// by a non-space, the closing `$` is preceded by a non-space and not followed
/// by a digit. "$5 and $10" fails on the space before the second `$`. A `$$…$$`
/// span inside a line is unambiguous and only needs to be non-empty.
fn is_inline_formula(source: &str, value: &str, next: Option<char>) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    if source.starts_with("$$") {
        return true;
    }
    let starts_tight = !value.starts_with(char::is_whitespace);
    let ends_tight = !value.ends_with(char::is_whitespace);
    starts_tight && ends_tight && !next.is_some_and(|c| c.is_ascii_digit())
}

/// Whether a `$$` block's source ends in its own closing fence, rather than
/// running on to the end of a document that is still arriving.
fn is_closed_block(source: &str) -> bool {
    let source = source.trim_end();
    let Some((_, last)) = source.rsplit_once('\n') else {
        return false;
    };
    // Inside a blockquote each line keeps its `>` markers.
    let last = last.trim_start_matches(|c: char| c == '>' || c.is_whitespace());
    last.starts_with("$$") && last.chars().all(|c| c == '$')
}

/// Whether RaTeX can parse and lay out `latex`. Run at parse time, off the
/// main thread, so a formula that cannot render falls back to its source.
fn typesets(latex: &str) -> bool {
    let Ok(nodes) = ratex_parser::parse(latex) else {
        return false;
    };
    validate_layout(&layout(&nodes, &LayoutOptions::default())).is_ok()
}

/// One formula rasterized for one size, scale and color.
struct Raster {
    image: Arc<RenderImage>,
    /// Logical size, in pixels.
    width: f32,
    height: f32,
    /// Distance from the top edge to the formula's baseline, in logical pixels.
    baseline: f32,
}

fn image(raster: &Raster) -> Img {
    img(raster.image.clone())
        .w(px(raster.width))
        .h(px(raster.height))
        .flex_none()
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RasterKey {
    latex: String,
    display: bool,
    font_size: u32,
    scale: u32,
    color: u32,
}

type Cache = Mutex<HashMap<RasterKey, Option<Arc<Raster>>>>;

/// The cached raster for a formula, typesetting it on first use. `None` when
/// it cannot render, which is cached too so a bad formula is tried once.
fn raster(
    latex: &str,
    display: bool,
    font_size: f32,
    scale: f32,
    color: Hsla,
) -> Option<Arc<Raster>> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let rgba = Rgba::from(color);
    let key = RasterKey {
        latex: latex.to_string(),
        display,
        font_size: font_size.to_bits(),
        scale: scale.to_bits(),
        color: u32::from_be_bytes(
            [rgba.r, rgba.g, rgba.b, rgba.a].map(|c| (c * 255.).round() as u8),
        ),
    };
    let cache = CACHE.get_or_init(Default::default);
    let hit = cache.lock().ok()?.get(&key).cloned();
    let rendered = match hit {
        Some(hit) => hit,
        None => {
            #[cfg(feature = "automation")]
            RASTERIZED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let rendered = rasterize(latex, display, font_size, scale, rgba).map(Arc::new);
            let mut cache = cache.lock().ok()?;
            if cache.len() >= CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(key.clone(), rendered.clone());
            rendered
        }
    };
    #[cfg(feature = "automation")]
    if rendered.is_some()
        && let Ok(mut drawn) = DRAWN.lock()
        && !drawn.contains(&key)
    {
        drawn.push(key);
    }
    rendered
}

/// Every formula drawn as an image so far, and how many were typeset (cache
/// misses), for tests to tell drawn math from literal text.
#[cfg(feature = "automation")]
static DRAWN: Mutex<Vec<RasterKey>> = Mutex::new(Vec::new());
#[cfg(feature = "automation")]
static RASTERIZED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "automation")]
pub fn automation_report() -> serde_json::Value {
    let drawn = DRAWN.lock().map(|drawn| drawn.clone()).unwrap_or_default();
    serde_json::json!({
        "drawn": drawn.iter().map(|key| serde_json::json!({
            "latex": key.latex,
            "display": key.display,
            "color": format!("{:08x}", key.color),
        })).collect::<Vec<_>>(),
        "rasterized": RASTERIZED.load(std::sync::atomic::Ordering::Relaxed),
    })
}

/// Typesets `latex` and rasterizes it at `scale` device pixels per logical
/// pixel, glyphs in `color` on a transparent background.
fn rasterize(
    latex: &str,
    display: bool,
    font_size: f32,
    scale: f32,
    color: Rgba,
) -> Option<Raster> {
    let nodes = ratex_parser::parse(latex).ok()?;
    let options = LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        color: Color {
            r: color.r,
            g: color.g,
            b: color.b,
            a: color.a,
        },
        ..Default::default()
    };
    let lbox = layout(&nodes, &options);
    validate_layout(&lbox).ok()?;
    let list = to_display_list(&lbox);
    let png = render_to_png(
        &list,
        &RenderOptions {
            font_size,
            padding: PAD,
            background_color: Color {
                r: 0.,
                g: 0.,
                b: 0.,
                a: 0.,
            },
            device_pixel_ratio: scale,
            ..Default::default()
        },
    )
    .ok()?;
    let mut pixels = image::load_from_memory_with_format(&png, ImageFormat::Png)
        .ok()?
        .into_rgba8();
    // gpui's RenderImage holds BGRA.
    for px in pixels.pixels_mut() {
        px.0.swap(0, 2);
    }
    let (w, h) = pixels.dimensions();
    Some(Raster {
        image: Arc::new(RenderImage::new(vec![Frame::new(pixels)])),
        width: w as f32 / scale,
        height: h as f32 / scale,
        baseline: PAD + list.height as f32 * font_size,
    })
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` macro would shadow `#[test]`.
    use std::sync::Arc;

    use super::{
        Formula, PAD, claims_block, formula_node, inline_formula, is_closed_block,
        is_inline_formula, raster, rasterize,
    };
    use gpui_kit::{Rgba, black, red};

    const INK: Rgba = Rgba {
        r: 0.,
        g: 0.,
        b: 0.,
        a: 1.,
    };

    #[test]
    fn single_dollar_spans_follow_pandoc() {
        assert!(is_inline_formula("$x$", "x", None));
        // Only a digit after the closing dollar makes it prose: "$n$th" is math.
        assert!(is_inline_formula("$n$", "n", Some('t')));
        assert!(!is_inline_formula("$ x$", " x", None));
        assert!(!is_inline_formula("$x $", "x ", None));
        // "$5$0" is a price.
        assert!(!is_inline_formula("$5$", "5", Some('0')));
    }

    #[test]
    fn double_dollars_inline_are_formulas() {
        assert!(is_inline_formula("$$ x $$", " x ", None));
        assert!(!is_inline_formula("$$  $$", "  ", None));
    }

    #[test]
    fn blocks_wait_for_their_closing_fence() {
        assert!(is_closed_block("$$\na\nb\n  $$  \n"));
        assert!(is_closed_block("$$\n> x\n> $$"), "inside a blockquote");
        assert!(!is_closed_block("$$"));
        // Mid-stream: half a closing fence, or a body line that only starts like one.
        assert!(!is_closed_block("$$\nx\n$"));
        assert!(!is_closed_block("$$\nx\n$$ y"));
    }

    #[test]
    fn parse_claims_only_formulas_that_typeset() {
        let doc = "$x$ and $$y$$";
        assert_eq!(inline_formula(doc, 0..3, "x"), Some(false));
        assert_eq!(inline_formula(doc, 8..13, "y"), Some(true));
        // The character after the span is read from the document.
        assert_eq!(inline_formula("costs $5$0", 6..9, "5"), None);
        assert_eq!(inline_formula(r"$\frac{1}{$", 0..11, r"\frac{1}{"), None);
        assert!(claims_block("$$\nx\n$$", "x"));
        assert!(!claims_block("$$\nx", "x"), "still streaming");
        assert!(!claims_block("$$\n \n$$", " "));
        assert!(!claims_block("$$\n\\frac{1}{\n$$", r"\frac{1}{"));
    }

    #[test]
    fn rasterizes_in_color_on_transparent() {
        let half_red = Rgba {
            r: 1.,
            g: 0.,
            b: 0.,
            a: 0.5,
        };
        let raster = rasterize(r"\frac{1}{2}", true, 16., 2., half_red).expect("renders");
        let frame = raster.image.as_bytes(0).expect("one frame");
        let pixels = frame.as_chunks::<4>().0;
        // BGRA: a transparent corner, nothing but red, and the color's alpha kept.
        assert_eq!(pixels[0][3], 0, "corner is transparent");
        assert!(pixels.iter().all(|px| px[0] == 0 && px[1] == 0));
        assert!(pixels.iter().any(|px| px[2] > 0), "draws glyphs");
        assert!(pixels.iter().all(|px| px[3] < 255));
    }

    #[test]
    fn display_style_is_larger() {
        let display = rasterize(r"\frac{1}{2}", true, 16., 1., INK).unwrap();
        let text = rasterize(r"\frac{1}{2}", false, 16., 1., INK).unwrap();
        assert!(display.height > text.height);
    }

    #[test]
    fn geometry_is_logical_and_baseline_is_on_the_formula() {
        // KaTeX's math italic `x` and `y` share a height; only `y` has depth.
        let x = rasterize("x", false, 20., 2., INK).unwrap();
        let y = rasterize("y", false, 20., 2., INK).unwrap();
        let x1 = rasterize("x", false, 20., 1., INK).unwrap();
        let x40 = rasterize("x", false, 40., 2., INK).unwrap();
        assert!(x.image.size(0).width.0 > x1.image.size(0).width.0);
        assert!((x.width - x1.width).abs() <= 1.);
        assert!((x.height - x1.height).abs() <= 1.);
        assert_eq!(x.baseline, x1.baseline, "logical, not device, pixels");
        assert_eq!(x.baseline, y.baseline, "depth hangs below the baseline");
        assert!(y.height > x.height);
        // Padding, then the formula's height at its font size.
        assert!(((x40.baseline - PAD) - 2. * (x.baseline - PAD)).abs() < 0.01);
    }

    #[test]
    fn raster_cache_keys_on_every_input() {
        let base = raster("x", false, 16., 1., black()).unwrap();
        assert!(Arc::ptr_eq(
            &base,
            &raster("x", false, 16., 1., black()).unwrap()
        ));
        for other in [
            raster("y", false, 16., 1., black()),
            raster("x", true, 16., 1., black()),
            raster("x", false, 17., 1., black()),
            raster("x", false, 16., 2., black()),
            raster("x", false, 16., 1., red()),
        ] {
            assert!(!Arc::ptr_eq(&base, &other.unwrap()));
        }
    }

    #[test]
    fn formula_node_copies_as_its_source() {
        let node = formula_node("math-inline", "x^2", "$$x^2$$", true);
        assert_eq!(node.as_text(), "$$x^2$$");
        assert_eq!(node.as_markdown(), "$$x^2$$");
        assert_eq!(node.accessibility_name(), "x^2");
        let formula = node.data::<Formula>().unwrap();
        assert_eq!(formula.latex, "x^2");
        assert!(formula.display);
    }
}
