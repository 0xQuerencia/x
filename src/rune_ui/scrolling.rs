//! Shared scrolling-body layout for the window's panes and cards.

use rune::{El, ScrollSpec, col};

/// How far a scroll body's top and bottom edges fade out.
const SCROLL_FADE: (f32, f32) = (4.0, 24.0);

/// A vertically scrolling body whose edges fade out. At rest its content
/// sits clear of the fades, so the first and last borders, rings, and fills
/// are never cut; content needing room beside the edges (bordered cards)
/// pads itself horizontally.
pub(super) fn scroll_body(key: &str, content: El) -> El {
    faded_body(key, content, SCROLL_FADE)
}

/// A [`scroll_body`] with its own top and bottom fades.
pub(super) fn faded_body(key: &str, content: El, (top, bottom): (f32, f32)) -> El {
    col()
        .key(key)
        .grow(1.0)
        .shrink()
        // Shrinks below its content in a fixed-height parent (#906).
        .min_h(0.0)
        .clip_fade(0.0, top, 0.0, bottom)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(col().key("inset").pt(top).pb(bottom).child(content))
}
