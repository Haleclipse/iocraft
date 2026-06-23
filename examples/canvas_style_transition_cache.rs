//! Demonstrates the opt-in canvas style transition cache.
//!
//! CC Ink's packed `StylePool` caches serialized SGR transitions by style ID.
//! iocraft keeps styles typed and explicit; custom renderers that emit many
//! repeated SGR transitions can use this cache without adopting packed screen
//! internals or changing the default `Canvas` writer.

use iocraft::prelude::*;

fn main() {
    let normal = CanvasResolvedStyle::default();
    let mut accent_text = CanvasTextStyle::default();
    accent_text.color = Some(Color::Yellow);
    accent_text.weight = Weight::Bold;
    accent_text.underline = true;
    let accent = CanvasResolvedStyle {
        text: accent_text,
        background_color: Some(Color::Blue),
    };

    let mut cache = CanvasStyleTransitionCache::new();
    let enter = cache.transition(normal, accent).to_string();
    let leave = cache.transition(accent, normal).to_string();

    println!("enter accent SGR: {:?}", enter);
    println!("leave accent SGR: {:?}", leave);
    println!("cached transition pairs: {}", cache.len());
}
