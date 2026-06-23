//! Demonstrates the Rust-native counterpart to CC Ink's
//! `ScrollBoxHandle.scrollToElement(el, offset)`.
//!
//! iocraft does not expose React/DOM nodes. Instead, pair
//! `use_component_rect()` with `ScrollBoxHandle::scroll_to_element(rect, offset)`
//! or use the pure `scroll_content_top_for_absolute_rect(...)` helper when you
//! already have the measured absolute rect.

use iocraft::{prelude::*, taffy::Rect};

fn main() {
    let current_scroll_top = 4;
    let viewport_top = 6;
    let target_rect = Rect {
        left: 0,
        right: 10,
        top: 18,
        bottom: 19,
    };

    let target_scroll_top =
        scroll_content_top_for_absolute_rect(current_scroll_top, viewport_top, target_rect, -1);

    println!("current scrollTop: {current_scroll_top}");
    println!("viewport top row:   {viewport_top}");
    println!("target rect top:    {}", target_rect.top);
    println!("offset:             -1");
    println!("next scrollTop:     {target_scroll_top}");
    println!();
    println!("In a component, call handle.write().scroll_to_element(rect, offset)");
    println!("with a rect from hooks.use_component_rect().");
}
