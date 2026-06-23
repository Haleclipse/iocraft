//! Demonstrates `ComponentDrawer::content_size()`, `visible_size()`, and
//! `remaining_canvas_size()`.
//!
//! `content_size()` mirrors CC Ink's `get-max-width.ts` idea for custom
//! components: the drawable content box is the computed node size minus padding
//! and border. `visible_size()` reports the current clipped intersection, while
//! `remaining_canvas_size()` mirrors CC Ink's text-renderer screen-edge clamp.

use iocraft::prelude::*;
use iocraft::taffy;

#[derive(Default, Props)]
struct ContentBoxProbeProps;

#[derive(Default)]
struct ContentBoxProbe;

impl Component for ContentBoxProbe {
    type Props<'a> = ContentBoxProbeProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_layout_style(taffy::style::Style {
            size: taffy::Size {
                width: taffy::style::Dimension::length(24.0),
                height: taffy::style::Dimension::length(5.0),
            },
            padding: taffy::Rect {
                left: taffy::style::LengthPercentage::length(2.0),
                right: taffy::style::LengthPercentage::length(2.0),
                top: taffy::style::LengthPercentage::length(1.0),
                bottom: taffy::style::LengthPercentage::length(1.0),
            },
            border: taffy::Rect {
                left: taffy::style::LengthPercentage::length(1.0),
                right: taffy::style::LengthPercentage::length(1.0),
                top: taffy::style::LengthPercentage::length(1.0),
                bottom: taffy::style::LengthPercentage::length(1.0),
            },
            ..Default::default()
        });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        let content = drawer.content_size();
        let visible = drawer.visible_size();
        let remaining = drawer.remaining_canvas_size();
        drawer.canvas().set_text(
            0,
            0,
            &format!(
                "c{}x{} v{} r{}",
                content.width, content.height, visible.width, remaining.width
            ),
            CanvasTextStyle::default(),
        );
    }
}

fn main() {
    element! {
        View(width: 16, height: 5) {
            ContentBoxProbe
        }
    }
    .print();
}
