//! Demonstrates retained screen-buffer region operations.
//!
//! The CC Ink fork uses screen-level metadata (damage, noSelect, softWrap) plus
//! bulk region operations (clear/blit) so renderer optimizations stay correct.
//! This example exercises the same iocraft primitives in a deterministic way.

use iocraft::prelude::*;
use std::io;

fn write_text(canvas: &mut Canvas, x: isize, y: isize, text: &str) {
    canvas
        .subview_mut(0, 0, 0, 0, canvas.width(), canvas.height())
        .set_text(x, y, text, CanvasTextStyle::default());
}

fn main() -> io::Result<()> {
    let mut source = Canvas::new(32, 5);
    write_text(&mut source, 0, 0, "header row");
    write_text(&mut source, 0, 1, "selectable content");
    write_text(&mut source, 0, 2, "soft wrap begins ");
    write_text(&mut source, 0, 3, "and continues");

    // noSelect metadata is used by selection/copy/search, but it does not
    // change terminal output or Canvas equality.
    source.mark_no_select_region(0, 1, 4, 1);

    // Row 3 is a visual continuation of row 2. Copy helpers join these rows
    // without adding a newline.
    source.mark_soft_wrap_continuation(3, "soft wrap begins ".len());

    // Post-render overlays participate in diffing. Use inverse here so the
    // effect is visible in a plain ANSI dump.
    source.set_overlay(0, 0, StyleOverlay::inverse());

    let mut retained = Canvas::new(32, 5);
    retained.blit_region_from(&source, 0, 0, 20, 4);
    println!(
        "After blit_region_from: damage={:?}",
        retained.damage_region()
    );
    println!(
        "noSelect copied at row 1 col 2: {}",
        retained.is_no_select(2, 1)
    );
    println!(
        "softWrap continuation row 3 previous content end: {}",
        retained.soft_wrap_continuation(3)
    );
    println!(
        "overlay copied at row 0 col 0: invert={}",
        retained.resolved_text_style(0, 0).unwrap().invert
    );

    let selected = retained.selected_text(SelectionRange::new(
        SelectionPoint { col: 0, row: 2 },
        SelectionPoint { col: 31, row: 3 },
    ));
    println!("soft-wrap selected text: {selected:?}");

    retained.clear_damage();
    retained.clear_region(0, 1, 9, 1);
    println!("After clear_region: damage={:?}", retained.damage_region());
    println!(
        "The terminal renderer uses damage.x as the sparse row-diff start column, matching CC Ink's damage-bounded diffEach scans."
    );
    println!(
        "Visible text after clear_region:\n{}",
        retained.get_text(0, 0, 32, 4)
    );

    retained.clear_damage();
    retained
        .subview_mut(4, 2, 0, 0, 32, 5)
        .clear_region(5, 0, 4, 1);
    println!(
        "Subview clear_region translates to root damage={:?}",
        retained.damage_region()
    );

    // Boundary cleanup: clearing a region that starts on the tail of a wide
    // grapheme or ends on the head repairs the adjacent cell and expands damage.
    let mut wide = Canvas::new(8, 1);
    write_text(&mut wide, 1, 0, "中x中");
    wide.clear_region(2, 0, 3, 1);
    println!("wide-boundary clear damage={:?}", wide.damage_region());
    println!(
        "wide-boundary text after clear={:?}",
        wide.get_text(0, 0, 8, 1)
    );

    // Right-edge wide graphemes are represented as skipped spacer-head
    // placeholders, matching CC Ink's screen/output behavior: the wide glyph is
    // not emitted because writing it at the last column would put the terminal
    // into pending-wrap.
    let mut edge = Canvas::new(4, 1);
    write_text(&mut edge, 3, 0, "中");
    let mut edge_ansi = Vec::new();
    edge.write_ansi(&mut edge_ansi)?;
    println!(
        "right-edge wide text={:?}, ansi contains glyph={}",
        edge.get_text(0, 0, 4, 1),
        String::from_utf8_lossy(&edge_ansi).contains('中')
    );

    println!("\nANSI dump of retained buffer:\n");
    retained.write_ansi(&mut io::stdout())?;
    Ok(())
}
