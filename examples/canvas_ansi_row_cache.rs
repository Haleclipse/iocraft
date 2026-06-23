//! Demonstrates opt-in ANSI row serialization caching.
//!
//! CC Ink keeps an Output-level character cache across frames. In iocraft this
//! helper is explicit: custom renderers can cache serialized Canvas rows without
//! changing the default writer or exposing packed screen/style IDs.

use iocraft::prelude::*;

fn main() -> std::io::Result<()> {
    let mut canvas = Canvas::new(24, 1);
    let mut style = CanvasTextStyle::default();
    style.color = Some(Color::Cyan);
    style.weight = Weight::Bold;
    canvas
        .subview_mut(0, 0, 0, 0, 24, 1)
        .set_text(0, 0, "cached row", style);

    let mut cache = CanvasAnsiRowCache::new();
    let mut first = Vec::new();
    cache.write_row(&canvas, 0, &mut first)?;

    let mut second = Vec::new();
    cache.write_row(&canvas, 0, &mut second)?;

    println!("cache entries: {}", cache.len());
    println!("row bytes reused: {}", first == second);
    Ok(())
}
