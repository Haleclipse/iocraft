use super::*;
use crate::prelude::*;
use crossterm::QueueableCommand;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Clone, Default)]
struct TestWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_terminal_set_clipboard_writes_raw_osc52_without_sync_update() {
    let stdout = TestWriter::default();
    let stderr = TestWriter::default();
    let mut terminal = Terminal::new(
        Box::new(stdout.clone()),
        Box::new(stderr),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    stdout.buf.lock().unwrap().clear(); // discard setup cursor-hide bytes

    terminal.set_clipboard("copy").unwrap();

    let output = String::from_utf8(stdout.buf.lock().unwrap().clone()).unwrap();
    assert_eq!(output, "\x1b]52;c;Y29weQ==\x07");
    assert!(
        !output.contains("\x1b[?2026"),
        "clipboard writes are not screen diffs and must not start synchronized update"
    );
}

#[test]
fn test_terminal_control_sequence_writes_raw_without_sync_update() {
    let stdout = TestWriter::default();
    let stderr = TestWriter::default();
    let mut terminal = Terminal::new(
        Box::new(stdout.clone()),
        Box::new(stderr),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    stdout.buf.lock().unwrap().clear(); // discard setup cursor-hide bytes

    terminal.write_control_sequence("\x1b]9;4;3;\x07").unwrap();

    let output = String::from_utf8(stdout.buf.lock().unwrap().clone()).unwrap();
    assert_eq!(output, "\x1b]9;4;3;\x07");
    assert!(
        !output.contains("\x1b[?2026"),
        "terminal notifications/progress are side-band controls, not screen diffs"
    );
}

fn selection_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
    }
}

#[test]
fn test_selection_controller_routes_shift_nav_key_to_focus_move() {
    let mut controller = SelectionController::new();
    controller.selection_mut().start(1, 1);
    controller.selection_mut().update(0, 1);

    let outcome = controller.handle_fullscreen_key_event(
        &selection_key(KeyCode::Left, KeyModifiers::SHIFT),
        4,
        3,
    );

    assert_eq!(
        outcome,
        FullscreenSelectionKeyOutcome::FocusMoved {
            movement: SelectionFocusMove::Left,
            moved: true,
        }
    );
    assert_eq!(
        controller.selection().focus(),
        Some(crate::canvas::SelectionPoint { col: 3, row: 0 })
    );
}

#[test]
fn test_selection_controller_key_event_copy_clear_and_preserve_paths() {
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(1, 0);

    assert_eq!(
        controller.handle_fullscreen_key_event(
            &selection_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            8,
            1,
        ),
        FullscreenSelectionKeyOutcome::CopyRequested
    );
    assert!(
        controller.has_selection(),
        "copy request should leave clear policy to caller"
    );

    assert_eq!(
        controller.handle_fullscreen_key_event(
            &selection_key(KeyCode::PageUp, KeyModifiers::SHIFT),
            8,
            1,
        ),
        FullscreenSelectionKeyOutcome::Preserved
    );
    assert!(controller.has_selection());

    assert_eq!(
        controller.handle_fullscreen_key_event(
            &selection_key(KeyCode::Right, KeyModifiers::empty()),
            8,
            1,
        ),
        FullscreenSelectionKeyOutcome::Cleared
    );
    assert!(!controller.has_selection());
}

#[test]
fn test_selection_controller_key_event_escape_and_release_handling() {
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(1, 0);
    let release = KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::empty(),
        kind: KeyEventKind::Release,
    };
    assert_eq!(
        controller.handle_fullscreen_key_event(&release, 8, 1),
        FullscreenSelectionKeyOutcome::Ignored
    );
    assert!(controller.has_selection());

    assert_eq!(
        controller.handle_fullscreen_key_event(
            &selection_key(KeyCode::Esc, KeyModifiers::empty()),
            8,
            1,
        ),
        FullscreenSelectionKeyOutcome::Cleared
    );
    assert!(!controller.has_selection());
}

#[test]
fn test_selection_controller_routes_fullscreen_mouse_press_drag_release() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    let press = FullscreenMouseEvent {
        modifiers: KeyModifiers::ALT,
        column: 1,
        row: 0,
        cell_is_blank: false,
        kind: MouseEventKind::Down(event::MouseButton::Left),
    };
    let outcome = controller.handle_fullscreen_mouse_event(&canvas, &press, 1_000, false);
    assert!(matches!(
        outcome,
        FullscreenSelectionEventOutcome::Press(SelectionPressOutcome {
            kind: crate::canvas::SelectionMousePressKind::Single,
            finished_previous_drag: false,
            cancel_pending_hyperlink: false,
        })
    ));
    assert!(controller.selection().last_press_had_alt());

    let drag = FullscreenMouseEvent::new(MouseEventKind::Drag(event::MouseButton::Left), 3, 0);
    assert_eq!(
        controller.handle_fullscreen_mouse_event(&canvas, &drag, 1_010, false),
        FullscreenSelectionEventOutcome::Drag
    );
    assert_eq!(controller.selected_text(&canvas), "bcd");

    let release = FullscreenMouseEvent::new(MouseEventKind::Up(event::MouseButton::Left), 3, 0);
    let outcome = controller.handle_fullscreen_mouse_event(&canvas, &release, 1_020, false);
    assert!(matches!(
        outcome,
        FullscreenSelectionEventOutcome::Release(SelectionReleaseOutcome {
            was_dragging: true,
            click: None,
            hyperlink: None,
        })
    ));
    assert!(controller.has_selection());
    assert!(!controller.selection().is_dragging());
}

#[test]
fn test_selection_controller_routes_fullscreen_mouse_double_click_and_wheel() {
    let mut canvas = Canvas::new(16, 1);
    canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text(
        0,
        0,
        "one two three",
        CanvasTextStyle::default(),
    );
    let mut controller = SelectionController::new();
    let down = FullscreenMouseEvent::new(MouseEventKind::Down(event::MouseButton::Left), 4, 0);

    controller.handle_fullscreen_mouse_event(&canvas, &down, 1_000, false);
    let outcome = controller.handle_fullscreen_mouse_event(&canvas, &down, 1_200, false);
    assert!(matches!(
        outcome,
        FullscreenSelectionEventOutcome::Press(SelectionPressOutcome {
            kind: crate::canvas::SelectionMousePressKind::Double,
            finished_previous_drag: true,
            cancel_pending_hyperlink: true,
        })
    ));
    assert_eq!(controller.selected_text(&canvas), "two");

    let wheel = FullscreenMouseEvent::new(MouseEventKind::ScrollDown, 4, 0);
    assert_eq!(
        controller.handle_fullscreen_mouse_event(&canvas, &wheel, 1_250, false),
        FullscreenSelectionEventOutcome::Wheel {
            cleared_selection: true,
        }
    );
    assert!(!controller.has_selection());
}

#[test]
fn test_selection_controller_routes_no_button_motion_lost_release_and_hover_dedupe() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    let down = FullscreenMouseEvent::new(MouseEventKind::Down(event::MouseButton::Left), 1, 0);
    let drag = FullscreenMouseEvent::new(MouseEventKind::Drag(event::MouseButton::Left), 3, 0);
    controller.handle_fullscreen_mouse_event(&canvas, &down, 1_000, false);
    controller.handle_fullscreen_mouse_event(&canvas, &drag, 1_010, false);

    let moved = FullscreenMouseEvent::new(MouseEventKind::Moved, 3, 0);
    let outcome = controller.handle_fullscreen_mouse_event(&canvas, &moved, 1_020, false);
    assert!(matches!(
        outcome,
        FullscreenSelectionEventOutcome::Hover(SelectionHoverOutcome {
            finished_drag: true,
            hover: Some(crate::canvas::SelectionPoint { col: 3, row: 0 }),
        })
    ));
    let repeat = controller.handle_fullscreen_mouse_event(&canvas, &moved, 1_030, false);
    assert!(matches!(
        repeat,
        FullscreenSelectionEventOutcome::Hover(SelectionHoverOutcome {
            finished_drag: false,
            hover: None,
        })
    ));
}

#[test]
fn test_terminal_set_clipboard_with_multiplexer_wraps_osc52() {
    let stdout = TestWriter::default();
    let stderr = TestWriter::default();
    let mut terminal = Terminal::new(
        Box::new(stdout.clone()),
        Box::new(stderr),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    stdout.buf.lock().unwrap().clear();

    terminal
        .set_clipboard_with_multiplexer("copy", ClipboardMultiplexer::Tmux)
        .unwrap();

    let output = String::from_utf8(stdout.buf.lock().unwrap().clone()).unwrap();
    assert_eq!(output, "\x1bPtmux;\x1b\x1b]52;c;Y29weQ==\x07\x1b\\");
}

struct ResizeReassertTerminal {
    events: Option<BoxStream<'static, TerminalEvent>>,
    reasserts: Arc<AtomicUsize>,
    size: Option<(u16, u16)>,
    dest: io::Sink,
}

impl Write for ResizeReassertTerminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminalImpl for ResizeReassertTerminal {
    fn size(&self) -> Option<(u16, u16)> {
        self.size
    }

    fn set_size_from_resize_event(&mut self, width: u16, height: u16) {
        self.size = Some((width, height));
    }

    fn reassert_after_resize(&mut self) -> io::Result<()> {
        self.reasserts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn reassert_after_stdin_resume(&mut self) -> io::Result<()> {
        self.reasserts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn is_raw_mode_enabled(&self) -> bool {
        false
    }

    fn clear_canvas(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn write_canvas(&mut self, _prev: Option<&Canvas>, _canvas: &Canvas) -> io::Result<()> {
        Ok(())
    }

    fn event_stream(&mut self) -> io::Result<BoxStream<'static, TerminalEvent>> {
        Ok(self
            .events
            .take()
            .unwrap_or_else(|| stream::pending().boxed()))
    }

    fn dest(&mut self) -> &mut dyn Write {
        &mut self.dest
    }

    fn alt(&mut self) -> &mut dyn Write {
        &mut self.dest
    }
}

fn new_test_writer() -> (TestWriter, Arc<Mutex<Vec<u8>>>) {
    let writer = TestWriter::default();
    let buf = writer.buf.clone();
    (writer, buf)
}

#[test]
fn test_std_terminal() {
    // There's unfortunately not much here we can really test, but we'll do our best.
    // TODO: Is there a library we can use to emulate terminal input/output?
    let mut terminal = Terminal::new(
        Box::new(std::io::stdout()),
        Box::new(std::io::stderr()),
        Output::Stdout,
        false,
        true,
    )
    .unwrap();
    assert!(!terminal.is_raw_mode_enabled());
    assert!(!terminal.received_ctrl_c());
    assert!(!terminal.is_raw_mode_enabled());
    let canvas = Canvas::new(10, 1);
    terminal.write_canvas(None, &canvas).unwrap();
}

fn render_canvas_to_vt(canvas: &Canvas, cols: usize, rows: usize) -> avt::Vt {
    render_canvases_to_vt(&[canvas], cols, rows)
}

fn render_canvases_to_vt(canvases: &[&Canvas], cols: usize, rows: usize) -> avt::Vt {
    let mut buf = Vec::new();
    for (i, canvas) in canvases.iter().enumerate() {
        if i > 0 {
            super::clear_canvas_inline(&mut buf, canvases[i - 1].height() as _).unwrap();
        }
        canvas.write_ansi_without_final_newline(&mut buf).unwrap();
    }
    let mut vt = avt::Vt::new(cols, rows);
    vt.feed_str(&String::from_utf8(buf).unwrap());
    vt
}

#[test]
fn test_inline_rewrite_single_line_cursor() {
    let mut canvas = Canvas::new(10, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, "hello", CanvasTextStyle::default());

    let vt = render_canvas_to_vt(&canvas, 10, 5);

    assert_eq!(vt.line(0).text(), "hello     ");
    assert_eq!(vt.cursor().row, 0, "cursor should stay on the first row");

    // clear and rerender with new content
    let mut canvas2 = Canvas::new(10, 1);
    canvas2
        .subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, "world", CanvasTextStyle::default());

    let vt = render_canvases_to_vt(&[&canvas, &canvas2], 10, 5);

    assert_eq!(vt.line(0).text(), "world     ");
    assert_eq!(vt.cursor().row, 0);
}

#[test]
fn test_inline_rewrite_multi_line_cursor() {
    let mut canvas = Canvas::new(10, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "line1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "line3", CanvasTextStyle::default());

    let vt = render_canvas_to_vt(&canvas, 10, 5);

    assert_eq!(vt.line(0).text(), "line1     ");
    assert_eq!(vt.line(1).text(), "          ");
    assert_eq!(vt.line(2).text(), "line3     ");
    assert_eq!(
        vt.cursor().row,
        2,
        "cursor should be on the last content row"
    );

    // clear and rerender with fewer lines
    let mut canvas2 = Canvas::new(10, 2);
    canvas2
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "new1", CanvasTextStyle::default());
    canvas2
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "new2", CanvasTextStyle::default());

    let vt = render_canvases_to_vt(&[&canvas, &canvas2], 10, 5);

    assert_eq!(vt.line(0).text(), "new1      ");
    assert_eq!(vt.line(1).text(), "new2      ");
    assert_eq!(
        vt.line(2).text(),
        "          ",
        "old line 3 should be cleared"
    );
    assert_eq!(vt.cursor().row, 1);
}

#[test]
fn test_inline_rewrite_no_extra_blank_line() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "first", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "second", CanvasTextStyle::default());

    let vt = render_canvas_to_vt(&canvas, 10, 5);

    assert_eq!(vt.line(0).text(), "first     ");
    assert_eq!(vt.line(1).text(), "second    ");
    assert_eq!(vt.cursor().row, 1, "cursor stays on last content row");

    // clear and rerender
    let mut canvas2 = Canvas::new(10, 2);
    canvas2
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "third", CanvasTextStyle::default());
    canvas2
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "fourth", CanvasTextStyle::default());

    let vt = render_canvases_to_vt(&[&canvas, &canvas2], 10, 5);

    assert_eq!(vt.line(0).text(), "third     ");
    assert_eq!(vt.line(1).text(), "fourth    ");
    assert_eq!(vt.cursor().row, 1);
}

#[test]
fn test_fullscreen_diff_preserves_origin() {
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "first", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "second", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "first", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "changed", CanvasTextStyle::default());

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 1, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let mut setup = Vec::new();
    write!(setup, "log\r\n").unwrap();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "log       ");
    assert_eq!(vt.line(1).text(), "first     ");
    assert_eq!(vt.line(2).text(), "changed   ");
    assert_eq!(
        vt.cursor().row,
        2,
        "cursor should stay on the canvas bottom when terminal size is unknown"
    );
}

#[test]
fn test_fullscreen_decstbm_scroll_hint_shifts_prev_before_diff() {
    let mut prev = Canvas::new(10, 5);
    for (y, label) in ["top", "one", "two", "three", "bottom"].iter().enumerate() {
        prev.subview_mut(0, 0, 0, 0, 10, 5).set_text(
            0,
            y as isize,
            label,
            CanvasTextStyle::default(),
        );
    }

    let mut next = Canvas::new(10, 5);
    for (y, label) in ["top", "two", "three", "new", "bottom"].iter().enumerate() {
        next.subview_mut(0, 0, 0, 0, 10, 5).set_text(
            0,
            y as isize,
            label,
            CanvasTextStyle::default(),
        );
    }
    next.set_scroll_hint(crate::canvas::ScrollHint {
        top: 1,
        bottom: 3,
        delta: 1,
    });

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.size = Some((10, 5));
    term.prev_size_on_write = Some((10, 5));
    term.decstbm_safe = true;
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = diff_buf.lock().unwrap().clone();
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.contains("\x1b[2;4r\x1b[1S\x1b[r\x1b[H"),
        "expected DECSTBM + SU scroll patch; got {diff_str:?}"
    );
    assert!(diff_str.contains("new"), "edge row should be repainted");
    assert!(
        !diff_str.contains("two") && !diff_str.contains("three"),
        "stable shifted rows should not be rewritten after virtual prev shift: {diff_str:?}"
    );

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff);
    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());
    assert_eq!(vt.line(0).text(), "top       ");
    assert_eq!(vt.line(1).text(), "two       ");
    assert_eq!(vt.line(2).text(), "three     ");
    assert_eq!(vt.line(3).text(), "new       ");
    assert_eq!(vt.line(4).text(), "bottom    ");
}

#[test]
fn test_fullscreen_damage_preserves_decstbm_scroll_hint() {
    let mut prev = Canvas::new(10, 5);
    for (y, label) in ["top", "one", "two", "three", "bottom"].iter().enumerate() {
        prev.subview_mut(0, 0, 0, 0, 10, 5).set_text(
            0,
            y as isize,
            label,
            CanvasTextStyle::default(),
        );
    }

    let mut next = Canvas::new(10, 5);
    for (y, label) in ["top", "two", "three", "new", "bottom"].iter().enumerate() {
        next.subview_mut(0, 0, 0, 0, 10, 5).set_text(
            0,
            y as isize,
            label,
            CanvasTextStyle::default(),
        );
    }
    next.set_scroll_hint(crate::canvas::ScrollHint {
        top: 1,
        bottom: 3,
        delta: 1,
    });
    next.mark_damage(crate::canvas::DamageRegion {
        x: 0,
        y: 0,
        width: 10,
        height: 5,
    });

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.size = Some((10, 5));
    term.prev_size_on_write = Some((10, 5));
    term.decstbm_safe = true;
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        diff.contains("\x1b[2;4r\x1b[1S\x1b[r\x1b[H"),
        "damage should not disable atomic DECSTBM scroll optimization: {diff:?}"
    );
    assert!(
        diff.contains("two") && diff.contains("three") && diff.contains("new"),
        "full damage should repaint the scrolled region after the hardware scroll: {diff:?}"
    );
}

#[test]
fn test_fullscreen_decstbm_scroll_hint_requires_atomic_update() {
    let mut prev = Canvas::new(10, 3);
    prev.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "one", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "two", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "three", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 3);
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "two", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "three", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "four", CanvasTextStyle::default());
    next.set_scroll_hint(crate::canvas::ScrollHint {
        top: 0,
        bottom: 2,
        delta: 1,
    });

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.size = Some((10, 3));
    term.prev_size_on_write = Some((10, 3));
    term.decstbm_safe = false;
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff_str = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        !diff_str.contains("\x1b[1;3r") && !diff_str.contains("\x1b[1S"),
        "DECSTBM must be skipped without synchronized-output atomicity: {diff_str:?}"
    );
    assert!(
        diff_str.contains("two") && diff_str.contains("hree") && diff_str.contains("four"),
        "without hardware scroll, changed rows should be patched with sparse row diffs: {diff_str:?}"
    );

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(diff_str.as_bytes());
    let mut vt = avt::Vt::new(10, 3);
    vt.feed_str(&String::from_utf8(setup).unwrap());
    assert_eq!(vt.line(0).text(), "two       ");
    assert_eq!(vt.line(1).text(), "three     ");
    assert_eq!(vt.line(2).text(), "four      ");
}

#[test]
fn test_fullscreen_initial_write_parks_cursor_at_terminal_bottom_when_size_known() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "first", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "second", CanvasTextStyle::default());

    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, 0);
    term.size = Some((10, 5));
    term.write_canvas(None, &canvas).unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        !output.contains("\r\n"),
        "fullscreen initial paint should use absolute row writes, not LF-based rendering that can scroll alt-screen: {output:?}"
    );
    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&output);

    assert_eq!(vt.line(0).text(), "first     ");
    assert_eq!(vt.line(1).text(), "second    ");
    assert_eq!(
        vt.cursor().row,
        4,
        "fullscreen cursor should be parked at terminal bottom"
    );
}

#[test]
fn test_fullscreen_resize_full_clears_and_reanchors() {
    let style = CanvasTextStyle::default();
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "old0", style);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "old1", style);

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "new0", style);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "new1", style);

    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 1, prev.height() as _);
    term.prev_size_on_write = Some((12, 4));
    term.size = Some((10, 4));
    term.write_canvas(Some(&prev), &next).unwrap();
    assert_eq!(term.prev_canvas_top_row, 0);

    let diff = buf.lock().unwrap().clone();
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.contains("\x1b[2J"),
        "fullscreen resize should erase the whole screen before repaint: {diff_str:?}"
    );
    assert!(
        !diff_str.contains("\r\n"),
        "fullscreen resize repaint should use absolute row writes, not LF-based rendering that can scroll alt-screen: {diff_str:?}"
    );

    let mut setup = Vec::new();
    write!(setup, "LOG\r\n").unwrap();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff);

    let mut vt = avt::Vt::new(10, 4);
    vt.feed_str(&String::from_utf8(setup).unwrap());
    assert_eq!(vt.line(0).text(), "new0      ");
    assert_eq!(vt.line(1).text(), "new1      ");
    assert_eq!(vt.cursor().row, 3, "cursor should park at terminal bottom");
}

#[test]
fn test_fullscreen_clear_preserves_output_above() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "first", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "second", CanvasTextStyle::default());

    let (dest, clear_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 1, canvas.height() as _);
    term.clear_canvas().unwrap();

    let mut setup = Vec::new();
    write!(setup, "log\r\n").unwrap();
    canvas.write_ansi_without_final_newline(&mut setup).unwrap();
    write!(setup, "\r\ntail").unwrap();
    setup.queue(cursor::MoveTo(0, 0)).unwrap();
    setup.extend_from_slice(&clear_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "log       ");
    assert_eq!(vt.line(1).text(), "          ");
    assert_eq!(vt.line(2).text(), "          ");
    assert_eq!(vt.line(3).text(), "          ");
}

fn new_fullscreen_term(
    dest: TestWriter,
    prev_canvas_top_row: u16,
    prev_canvas_height: u16,
) -> StdTerminal<'static> {
    StdTerminal {
        input_is_terminal: false,
        dest: Box::new(dest),
        alt: Box::new(io::sink()),
        fullscreen: true,
        mouse_capture: false,
        dynamic_alternate_saved_mouse_capture: None,
        raw_mode_enabled: false,
        enabled_keyboard_enhancement: false,
        keyboard_enhancement_flags: event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
        prev_canvas_top_row,
        prev_canvas_height,
        size: None,
        prev_size_on_write: None,
        cursor_visible: false,
        cursor_displacement_rows: 0,
        inline_pending_wrap: false,
        decstbm_safe: false,
        inline_force_full_rewrite_next_diff: false,
        #[cfg(unix)]
        resume_signal: None,
    }
}

fn new_inline_term(dest: TestWriter, prev_canvas_height: u16) -> StdTerminal<'static> {
    new_inline_term_with_size(dest, prev_canvas_height, (10, 10))
}

fn new_inline_term_with_size(
    dest: TestWriter,
    prev_canvas_height: u16,
    term_size: (u16, u16),
) -> StdTerminal<'static> {
    StdTerminal {
        input_is_terminal: false,
        dest: Box::new(dest),
        alt: Box::new(io::sink()),
        fullscreen: false,
        mouse_capture: false,
        dynamic_alternate_saved_mouse_capture: None,
        raw_mode_enabled: false,
        enabled_keyboard_enhancement: false,
        keyboard_enhancement_flags: event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
        prev_canvas_top_row: 0,
        prev_canvas_height,
        size: Some(term_size),
        prev_size_on_write: None,
        cursor_visible: false,
        cursor_displacement_rows: 0,
        inline_pending_wrap: false,
        decstbm_safe: false,
        inline_force_full_rewrite_next_diff: false,
        #[cfg(unix)]
        resume_signal: None,
    }
}

/// Run an inline diff (prev → next) and return the raw diff bytes plus
/// an `avt::Vt` showing the final visible state.
fn inline_diff_vt(prev: &Canvas, next: &Canvas, term_size: (u16, u16)) -> (Vec<u8>, avt::Vt) {
    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, prev.height() as _, term_size);
    term.write_canvas(Some(prev), next).unwrap();

    let diff = diff_buf.lock().unwrap().clone();
    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff);

    let mut vt = avt::Vt::new(term_size.0 as _, term_size.1 as _);
    vt.feed_str(&String::from_utf8(setup).unwrap());
    (diff, vt)
}

#[test]
fn test_inline_pending_wrap_resolved_before_next_frame_relative_move() {
    let style = CanvasTextStyle::default();
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaaaaaaaaa", style);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "bbbbbbbbbb", style);

    let mut next = prev.clone();
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "cccccccccc", style);

    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, 0, (10, 5));
    term.write_canvas(None, &prev).unwrap();
    assert!(term.inline_pending_wrap);

    let first_frame = buf.lock().unwrap().clone();
    buf.lock().unwrap().clear();
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = buf.lock().unwrap().clone();
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.starts_with("\r\x1b[1F"),
        "pending wrap should be resolved with CR before moving up: {diff_str:?}"
    );

    let mut vt = avt::Vt::new(10, 5);
    let mut all = first_frame;
    all.extend_from_slice(&diff);
    vt.feed_str(&String::from_utf8(all).unwrap());
    assert_eq!(vt.line(0).text(), "cccccccccc");
    assert_eq!(vt.line(1).text(), "bbbbbbbbbb");
    assert_eq!(vt.cursor().row, 1);
}

#[test]
fn test_inline_pending_wrap_resolved_between_changed_rows() {
    let style = CanvasTextStyle::default();
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaaaaaaaaa", style);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "bbb", style);

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "cccccccccc", style);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "ddd", style);

    let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.contains("\r\x1b[1E"),
        "pending wrap should be resolved with CR before moving down: {diff_str:?}"
    );
    assert_eq!(vt.line(0).text(), "cccccccccc");
    assert_eq!(vt.line(1).text(), "ddd       ");
    assert_eq!(vt.cursor().row, 1);
}

#[test]
fn test_inline_pending_wrap_tracks_actual_rendered_width() {
    let style = CanvasTextStyle::default();
    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, 0, (10, 5));

    let mut short = Canvas::new(10, 2);
    short
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaaaaaaaaa", style);
    short
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "bbb", style);
    term.write_canvas(None, &short).unwrap();
    assert!(
        !term.inline_pending_wrap,
        "full-width canvas should not imply pending-wrap when the actual last row is short"
    );

    let mut full = short.clone();
    full.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "cccccccccc", style);
    buf.lock().unwrap().clear();
    term.write_canvas(Some(&short), &full).unwrap();
    assert!(
        term.inline_pending_wrap,
        "a row write that reaches the right margin should enter pending-wrap"
    );
}

/// Inline-mode cursor positioning: moving the cursor up to the declared row must be
/// undone (baseline restore) before the next canvas write, so the row-diff logic's
/// "cursor sits on the last row" assumption holds.
#[test]
fn test_clear_canvas_viewport_tall_inline_preserves_scrollback() {
    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, 5, (10, 5));

    term.clear_canvas().unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[2J"),
        "viewport-tall inline clear should erase visible screen: {output:?}"
    );
    assert!(
        !output.contains("\x1b[3J"),
        "clearing a main-screen live canvas must preserve native scrollback: {output:?}"
    );
    assert!(
        output.contains("\x1b[1;1H"),
        "viewport-tall inline clear should return cursor home: {output:?}"
    );
}

#[test]
fn test_clear_screen_resets_inline_state_without_purging_scrollback() {
    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term(dest, 3);
    term.cursor_displacement_rows = 1;
    term.inline_pending_wrap = true;
    term.inline_force_full_rewrite_next_diff = true;

    term.clear_screen().unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[2J"),
        "visible-screen clear should erase the screen: {output:?}"
    );
    assert!(
        !output.contains("\x1b[3J"),
        "visible-screen clear must preserve native terminal scrollback: {output:?}"
    );
    assert!(
        output.contains("\x1b[1;1H"),
        "visible-screen clear should return cursor home: {output:?}"
    );
    assert_eq!(term.prev_canvas_height, 0);
    assert_eq!(term.prev_canvas_top_row, 0);
    assert_eq!(term.prev_size_on_write, None);
    assert_eq!(term.cursor_displacement_rows, 0);
    assert!(!term.inline_pending_wrap);
    assert!(!term.inline_force_full_rewrite_next_diff);
}

#[test]
fn test_clear_terminal_full_resets_inline_state() {
    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term(dest, 3);
    term.cursor_displacement_rows = 1;
    term.inline_pending_wrap = true;
    term.inline_force_full_rewrite_next_diff = true;

    term.clear_terminal().unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[2J"),
        "full terminal clear should erase visible screen: {output:?}"
    );
    assert!(
        output.contains("\x1b[3J"),
        "full terminal clear should purge scrollback: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[2J\x1b[3J\x1b[H"),
        "full terminal clear should use the CC Ink modern clear sequence: {output:?}"
    );
    assert!(
        !output.contains("\x1b[?1049h") && !output.contains("\x1b[?1049l"),
        "main-screen clear must not enter/leave alternate-screen fullscreen mode: {output:?}"
    );
    assert_eq!(term.prev_canvas_height, 0);
    assert_eq!(term.prev_canvas_top_row, 0);
    assert_eq!(term.prev_size_on_write, None);
    assert_eq!(term.cursor_displacement_rows, 0);
    assert!(!term.inline_pending_wrap);
    assert!(!term.inline_force_full_rewrite_next_diff);
}

#[test]
fn test_position_cursor_inline_displacement_and_baseline_restore() {
    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term(dest, 3); // canvas height 3, last row = 2

    use crate::canvas::CursorDeclaration;

    // ink model: visible=false, physical cursor stays hidden — only positioned for IME.
    term.position_cursor(Some(CursorDeclaration {
        x: 4,
        y: 0,
        visible: false,
    }))
    .unwrap();
    assert_eq!(term.cursor_displacement_rows, 2);
    assert!(!term.cursor_visible);
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[2F"),
        "expected MoveToPreviousLine(2): {output:?}"
    );
    assert!(
        output.contains("\x1b[5G"),
        "expected MoveToColumn(4) (1-based CSI G): {output:?}"
    );
    assert!(
        !output.contains("\x1b[?25h"),
        "physical cursor should NOT be shown: {output:?}"
    );

    // position_cursor(None) is a no-op when cursor was never shown.
    buf.lock().unwrap().clear();
    term.position_cursor(None).unwrap();
    assert!(!term.cursor_visible);

    // The next baseline restore moves back down by the displacement.
    buf.lock().unwrap().clear();
    term.restore_cursor_baseline().unwrap();
    assert_eq!(term.cursor_displacement_rows, 0);
    term.dest.flush().unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[2E"),
        "expected MoveToNextLine(2): {output:?}"
    );
}

/// Fullscreen-mode cursor positioning uses absolute coordinates and leaves no
/// displacement to restore.
#[test]
fn test_position_cursor_fullscreen_absolute() {
    use crate::canvas::CursorDeclaration;
    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 5, 3); // top row 5
    term.position_cursor(Some(CursorDeclaration {
        x: 2,
        y: 1,
        visible: false,
    }))
    .unwrap();
    assert_eq!(term.cursor_displacement_rows, 0);
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    // MoveTo is 1-based in CSI: row 5+1+1=7, col 2+1=3.
    assert!(
        output.contains("\x1b[7;3H"),
        "expected absolute MoveTo: {output:?}"
    );
}

#[test]
fn test_position_cursor_fullscreen_clamps_to_terminal_size() {
    use crate::canvas::CursorDeclaration;
    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, 3);
    term.size = Some((5, 4));

    term.position_cursor(Some(CursorDeclaration {
        x: 99,
        y: 99,
        visible: false,
    }))
    .unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[4;5H"),
        "fullscreen cursor declarations should be clamped to terminal bounds: {output:?}"
    );
}

fn synchronized_output_env(pairs: &[(&str, &str)]) -> bool {
    is_synchronized_output_supported_with_env(|key| {
        pairs
            .iter()
            .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
    })
}

#[test]
fn test_synchronized_output_supported_matches_cc_gate() {
    assert!(synchronized_output_env(&[("TERM_PROGRAM", "iTerm.app")]));
    assert!(synchronized_output_env(&[("TERM_PROGRAM", "ghostty")]));
    assert!(synchronized_output_env(&[("TERM", "xterm-kitty")]));
    assert!(synchronized_output_env(&[("TERM", "foot-extra")]));
    assert!(synchronized_output_env(&[("WT_SESSION", "abc")]));
    assert!(synchronized_output_env(&[("VTE_VERSION", "6800")]));
    assert!(!synchronized_output_env(&[("VTE_VERSION", "6700")]));
    assert!(!synchronized_output_env(&[(
        "TMUX",
        "/tmp/tmux-1/default,1,0"
    )]));
}

fn extended_keys_env(pairs: &[(&str, &str)]) -> bool {
    supports_extended_keys_with_env(|key| {
        pairs
            .iter()
            .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
    })
}

#[test]
fn test_supports_extended_keys_matches_cc_allowlist() {
    assert!(extended_keys_env(&[("TERM_PROGRAM", "iTerm.app")]));
    assert!(extended_keys_env(&[("TERM", "xterm-kitty")]));
    assert!(extended_keys_env(&[("TERM", "xterm-ghostty")]));
    assert!(extended_keys_env(&[("TMUX", "/tmp/tmux-1/default,1,0")]));
    assert!(extended_keys_env(&[("WT_SESSION", "1")]));
    assert!(!extended_keys_env(&[("TERM_PROGRAM", "vscode")]));
    assert!(!extended_keys_env(&[("TERM_PROGRAM", "alacritty")]));
}

fn cursor_yank_env(is_windows: bool, pairs: &[(&str, &str)]) -> bool {
    has_cursor_up_viewport_yank_bug_with_env(is_windows, |key| {
        pairs
            .iter()
            .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
    })
}

#[test]
fn test_has_cursor_up_viewport_yank_bug_matches_cc_gate() {
    assert!(cursor_yank_env(true, &[]));
    assert!(cursor_yank_env(false, &[("WT_SESSION", "abc")]));
    assert!(!cursor_yank_env(false, &[]));
}

fn clear_sequence_env(is_windows: bool, pairs: &[(&str, &str)]) -> &'static str {
    clear_terminal_sequence_with_env(is_windows, |key| {
        pairs
            .iter()
            .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
    })
}

#[test]
fn test_clear_terminal_sequence_matches_cc_windows_gate() {
    let modern = "\x1b[2J\x1b[3J\x1b[H";
    let legacy_windows = "\x1b[2J\x1b[0f";

    assert_eq!(clear_sequence_env(false, &[]), modern);
    assert_eq!(clear_sequence_env(true, &[("WT_SESSION", "abc")]), modern);
    assert_eq!(
        clear_sequence_env(
            true,
            &[
                ("TERM_PROGRAM", "vscode"),
                ("TERM_PROGRAM_VERSION", "1.0.0")
            ]
        ),
        modern
    );
    assert_eq!(
        clear_sequence_env(true, &[("TERM_PROGRAM", "mintty")]),
        modern
    );
    assert_eq!(clear_sequence_env(true, &[("MSYSTEM", "MINGW64")]), modern);
    assert_eq!(clear_sequence_env(true, &[]), legacy_windows);
    assert_eq!(
        clear_sequence_env(true, &[("TERM_PROGRAM", "vscode")]),
        legacy_windows
    );
}

fn is_xterm_js_env(pairs: &[(&str, &str)], xtversion: Option<&str>) -> bool {
    is_xterm_js_with_env_and_xtversion(
        |key| {
            pairs
                .iter()
                .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
        },
        xtversion,
    )
}

#[test]
fn test_xtversion_detection_matches_cc_xterm_js_fallback() {
    assert_eq!(xtversion_query_sequence(), "\x1b[>0q");
    assert_eq!(
        parse_xtversion_response("\x1bP>|xterm.js(5.5.0)\x1b\\"),
        Some("xterm.js(5.5.0)")
    );
    assert_eq!(
        parse_xtversion_response("\x1bP>|ghostty 1.2.0\x07"),
        Some("ghostty 1.2.0")
    );
    assert_eq!(parse_xtversion_response("\x1b[?1;2c"), None);

    assert!(is_xterm_js_env(&[("TERM_PROGRAM", "vscode")], None));
    assert!(is_xterm_js_env(&[], Some("xterm.js(5.5.0)")));
    assert!(!is_xterm_js_env(&[], Some("ghostty 1.2.0")));
    assert!(!is_xterm_js_env(&[], None));
}

#[test]
fn test_terminal_input_tokenizer_matches_cc_ink_boundaries() {
    use TerminalInputToken::{Sequence, Text};

    let mut tokenizer = TerminalInputTokenizer::new();
    assert_eq!(tokenizer.feed("hi中\x1b["), vec![Text("hi中".to_string())]);
    assert_eq!(tokenizer.buffered(), "\x1b[");
    assert_eq!(
        tokenizer.feed("A!"),
        vec![Sequence("\x1b[A".to_string()), Text("!".to_string())]
    );
    assert_eq!(tokenizer.buffered(), "");

    assert!(tokenizer.feed("\x1b]11;rgb:0000").is_empty());
    assert_eq!(tokenizer.buffered(), "\x1b]11;rgb:0000");
    assert_eq!(
        tokenizer.feed("/0000/0000\x1b\\x"),
        vec![
            Sequence("\x1b]11;rgb:0000/0000/0000\x1b\\".to_string()),
            Text("x".to_string()),
        ]
    );

    assert!(tokenizer.feed("\x1b[?").is_empty());
    assert_eq!(tokenizer.flush(), vec![Sequence("\x1b[?".to_string())]);
    assert_eq!(tokenizer.buffered(), "");

    let mut legacy_mouse = TerminalInputTokenizer::with_x10_mouse(true);
    assert_eq!(
        legacy_mouse.feed("\x1b[Mabc"),
        vec![Sequence("\x1b[Mabc".to_string())]
    );

    let mut csi_delete_lines = TerminalInputTokenizer::new();
    assert_eq!(
        csi_delete_lines.feed("\x1b[Mabc"),
        vec![Sequence("\x1b[M".to_string()), Text("abc".to_string())]
    );
}

#[test]
fn test_terminal_input_tokenizer_feeds_response_parser_like_cc_ink() {
    let mut tokenizer = TerminalInputTokenizer::new();
    assert_eq!(
        tokenizer.feed_parsed("typed\x1b[?2026;1$y\x1bP>|xterm.js(5.5.0)\x1b\\"),
        vec![
            TerminalParsedInput::Text("typed".to_string()),
            TerminalParsedInput::Response(TerminalResponse::Decrpm {
                mode: 2026,
                status: DecrpmStatus::Set.code(),
            }),
            TerminalParsedInput::Response(TerminalResponse::Xtversion {
                name: "xterm.js(5.5.0)".to_string(),
            }),
        ]
    );

    let mut tokenizer = TerminalInputTokenizer::new();
    assert!(tokenizer.feed_parsed("\x1b[?").is_empty());
    assert_eq!(
        tokenizer.flush_parsed(),
        vec![TerminalParsedInput::Sequence("\x1b[?".to_string())]
    );
}

#[test]
fn test_terminal_input_tokenizer_groups_bracketed_paste_like_cc_ink() {
    let mut tokenizer = TerminalInputTokenizer::new();
    assert_eq!(
        tokenizer.feed_parsed("before\x1b[200~hello\x1b[31m\x1b[201~after"),
        vec![
            TerminalParsedInput::Text("before".to_string()),
            TerminalParsedInput::Paste("hello\x1b[31m".to_string()),
            TerminalParsedInput::Text("after".to_string()),
        ]
    );

    let mut chunked = TerminalInputTokenizer::new();
    assert!(chunked.feed_parsed("\x1b[200~hel").is_empty());
    assert_eq!(
        chunked.feed_parsed("lo\x1b[201~"),
        vec![TerminalParsedInput::Paste("hello".to_string())]
    );

    let mut empty = TerminalInputTokenizer::new();
    assert_eq!(
        empty.feed_parsed("\x1b[200~\x1b[201~"),
        vec![TerminalParsedInput::Paste(String::new())]
    );

    let mut flushed = TerminalInputTokenizer::new();
    assert!(flushed.feed_parsed("\x1b[200~partial").is_empty());
    assert_eq!(
        flushed.flush_parsed(),
        vec![TerminalParsedInput::Paste("partial".to_string())]
    );
}

#[test]
fn test_terminal_input_tokenizer_parses_sgr_mouse_like_cc_ink() {
    let mut tokenizer = TerminalInputTokenizer::new();
    assert_eq!(
        tokenizer.feed_parsed("\x1b[<0;12;3M\x1b[<32;12;3m"),
        vec![
            TerminalParsedInput::Mouse(TerminalParsedMouse {
                button: 0,
                action: TerminalParsedMouseAction::Press,
                column: 12,
                row: 3,
                sequence: "\x1b[<0;12;3M".to_string(),
            }),
            TerminalParsedInput::Mouse(TerminalParsedMouse {
                button: 32,
                action: TerminalParsedMouseAction::Release,
                column: 12,
                row: 3,
                sequence: "\x1b[<32;12;3m".to_string(),
            }),
        ]
    );

    let mut wheel = TerminalInputTokenizer::new();
    assert_eq!(
        wheel.feed_parsed("\x1b[<64;12;3M"),
        vec![TerminalParsedInput::Sequence("\x1b[<64;12;3M".to_string())],
        "wheel events stay as key-parser input like CC Ink"
    );

    let mut orphaned = TerminalInputTokenizer::new();
    assert_eq!(
        orphaned.feed_parsed("[<0;12;3M"),
        vec![TerminalParsedInput::Mouse(TerminalParsedMouse {
            button: 0,
            action: TerminalParsedMouseAction::Press,
            column: 12,
            row: 3,
            sequence: "\x1b[<0;12;3M".to_string(),
        })],
        "orphaned mouse tails are re-synthesized instead of leaking into text"
    );
}

#[test]
fn test_parse_terminal_key_sequence_matches_cc_ink_parse_keypress() {
    let shift_enter = parse_terminal_key_sequence("\x1b[13;2u");
    assert_eq!(shift_enter.name.as_deref(), Some("return"));
    assert!(shift_enter.shift);
    assert!(!shift_enter.ctrl);

    let alt_b = parse_terminal_key_sequence("\x1b[27;3;98~");
    assert_eq!(alt_b.name.as_deref(), Some("b"));
    assert!(alt_b.meta);
    assert!(!alt_b.shift);

    let ctrl_space = parse_terminal_key_sequence("\x1b[32;5u");
    assert_eq!(ctrl_space.name.as_deref(), Some("space"));
    assert!(ctrl_space.ctrl);

    let ctrl_left = parse_terminal_key_sequence("\x1b[1;5D");
    assert_eq!(ctrl_left.name.as_deref(), Some("left"));
    assert_eq!(ctrl_left.code, None);
    assert!(ctrl_left.ctrl);

    let upper = parse_terminal_key_sequence("A");
    assert_eq!(upper.name.as_deref(), Some("a"));
    assert!(upper.shift);

    let ctrl_c = parse_terminal_key_sequence("\x03");
    assert_eq!(ctrl_c.name.as_deref(), Some("c"));
    assert!(ctrl_c.ctrl);

    let return_key = parse_terminal_key_sequence("\r");
    assert_eq!(return_key.name.as_deref(), Some("return"));
    assert_eq!(return_key.raw, None);

    let natural_left = parse_terminal_key_sequence("\x1bb");
    assert_eq!(natural_left.name.as_deref(), Some("left"));
    assert!(natural_left.meta);

    let sgr_wheel = parse_terminal_key_sequence("\x1b[<64;12;3M");
    assert_eq!(sgr_wheel.name.as_deref(), Some("wheelup"));

    let x10_wheel = parse_terminal_key_sequence("\x1b[M`!!");
    assert_eq!(x10_wheel.name.as_deref(), Some("wheelup"));

    let unmapped = parse_terminal_key_sequence("\x1b[25~");
    assert_eq!(unmapped.name, None);
    assert_eq!(unmapped.code.as_deref(), Some("[25~"));
}

#[test]
fn test_parse_terminal_input_event_matches_cc_ink_input_event() {
    let enter = parse_terminal_input_event("\r");
    assert!(enter.key.return_key);
    assert_eq!(enter.input, "");

    let ctrl_space = parse_terminal_input_event("\x1b[32;5u");
    assert!(ctrl_space.key.ctrl);
    assert_eq!(ctrl_space.input, " ");

    let shift_enter = parse_terminal_input_event("\x1b[13;2u");
    assert!(shift_enter.key.return_key);
    assert!(shift_enter.key.shift);
    assert_eq!(shift_enter.input, "return");

    let alt_b = parse_terminal_input_event("\x1b[27;3;98~");
    assert!(alt_b.key.meta);
    assert_eq!(alt_b.input, "b");

    let keypad_zero = parse_terminal_input_event("\x1bOp");
    assert_eq!(keypad_zero.input, "0");

    let arrow = parse_terminal_input_event("\x1b[D");
    assert!(arrow.key.left_arrow);
    assert_eq!(arrow.input, "");

    let upper = parse_terminal_input_event("A");
    assert!(upper.key.shift);
    assert_eq!(upper.input, "A");

    let wheel = parse_terminal_input_event("\x1b[<64;12;3M");
    assert!(wheel.key.wheel_up);
    assert_eq!(wheel.input, "");

    let orphaned_mouse_tail = parse_terminal_input_event("[<64;12;3M");
    assert_eq!(orphaned_mouse_tail.input, "");

    let unmapped = parse_terminal_input_event("\x1b[25~");
    assert_eq!(unmapped.input, "");
}

#[test]
fn test_terminal_input_parser_exposes_cc_ink_flush_timing_state() {
    assert_eq!(TERMINAL_INPUT_NORMAL_TIMEOUT, Duration::from_millis(50));
    assert_eq!(TERMINAL_INPUT_PASTE_TIMEOUT, Duration::from_millis(500));

    let mut tokenizer = TerminalInputTokenizer::new();
    assert_eq!(tokenizer.pending_flush_timeout(), None);
    assert!(tokenizer.feed("\x1b[").is_empty());
    assert!(tokenizer.has_incomplete_sequence());
    assert!(!tokenizer.is_in_paste());
    assert_eq!(
        tokenizer.pending_flush_timeout(),
        Some(TERMINAL_INPUT_NORMAL_TIMEOUT)
    );
    assert!(tokenizer.should_flush_incomplete(false));
    assert!(!tokenizer.should_flush_incomplete(true));

    let mut parser = TerminalInputParser::new();
    assert!(parser.feed("\x1b[200~payload\x1b[").is_empty());
    assert!(parser.is_in_paste());
    assert!(parser.has_incomplete_sequence());
    assert_eq!(
        parser.pending_flush_timeout(),
        Some(TERMINAL_INPUT_PASTE_TIMEOUT)
    );
    assert!(!parser.should_flush_incomplete(true));
    assert!(parser.should_flush_incomplete(false));
    assert_eq!(
        parser.flush(),
        vec![TerminalParsedInput::Paste("payload\x1b[".to_string())]
    );
    assert!(!parser.is_in_paste());
    assert_eq!(parser.pending_flush_timeout(), None);
}

#[test]
fn test_terminal_input_bytes_match_cc_ink_input_to_string() {
    assert_eq!(terminal_input_bytes_to_string(&[0xe1]), "\x1ba");
    assert_eq!(terminal_input_bytes_to_string("é".as_bytes()), "é");

    let mut tokenizer = TerminalInputTokenizer::new();
    assert_eq!(
        tokenizer.feed_bytes(&[0xe1]),
        vec![TerminalInputToken::Sequence("\x1ba".to_string())]
    );

    let mut parser = TerminalInputParser::new();
    let parsed = parser.feed_bytes(&[0xe1]);
    assert!(matches!(
        parsed.as_slice(),
        [TerminalParsedInput::Key(event)]
            if event.input == "a" && event.key.meta && event.keypress.meta
    ));

    let mut parser = TerminalInputParser::new();
    let events = parser.feed_bytes_events(&[0xe1]);
    assert!(matches!(
        events.as_slice(),
        [TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers,
            kind: KeyEventKind::Press,
        })] if modifiers.contains(KeyModifiers::ALT)
    ));

    let mut parser = TerminalInputParser::new();
    assert_eq!(
        parser.feed_bytes(b"\x1b[200~pasted\x1b[201~"),
        vec![TerminalParsedInput::Paste("pasted".to_string())]
    );

    let mut responses = TerminalResponseParser::new();
    assert_eq!(
        responses.feed_bytes(b"\x1bP>|xterm.js(5.5.0)\x1b\\"),
        vec![TerminalResponse::Xtversion {
            name: "xterm.js(5.5.0)".to_string()
        }]
    );
}

#[test]
fn test_terminal_input_parser_matches_cc_ink_parse_multiple_keypresses() {
    let mut parser = TerminalInputParser::new();
    let parsed =
        parser.feed("a\x1b[A\x1b[?2026;1$y\x1b[200~hi\x1b[31m\x1b[201~\x1b[<0;12;3M\x1b[<64;12;3M");

    assert!(matches!(
        &parsed[0],
        TerminalParsedInput::Key(event)
            if event.input == "a" && event.keypress.name.as_deref() == Some("a")
    ));
    assert!(matches!(
        &parsed[1],
        TerminalParsedInput::Key(event) if event.key.up_arrow && event.input.is_empty()
    ));
    assert!(matches!(
        &parsed[2],
        TerminalParsedInput::Response(TerminalResponse::Decrpm { mode: 2026, status })
            if *status == DecrpmStatus::Set.code()
    ));
    assert_eq!(
        parsed[3],
        TerminalParsedInput::Paste("hi\x1b[31m".to_string())
    );
    assert!(matches!(
        &parsed[4],
        TerminalParsedInput::Mouse(mouse)
            if mouse.button == 0 && mouse.column == 12 && mouse.row == 3
    ));
    assert!(matches!(
        &parsed[5],
        TerminalParsedInput::Key(event) if event.key.wheel_up && event.input.is_empty()
    ));

    let mut orphaned = TerminalInputParser::new();
    let parsed = [orphaned.feed("[<64;12;3M"), orphaned.feed("[M`!!")].concat();
    assert!(matches!(
        &parsed[0],
        TerminalParsedInput::Key(event) if event.key.wheel_up && event.input.is_empty()
    ));
    assert!(matches!(
        &parsed[1],
        TerminalParsedInput::Key(event) if event.key.wheel_up && event.input.is_empty()
    ));

    let mut incomplete = TerminalInputParser::new();
    assert!(incomplete.feed("\x1b[?").is_empty());
    assert!(matches!(
        incomplete.flush().as_slice(),
        [TerminalParsedInput::Key(event)] if event.input == "[?"
    ));
}

#[test]
fn test_terminal_input_parser_event_bridge_feeds_iocraft_events() {
    let mut parser = TerminalInputParser::new();
    let events = parser
        .feed_events("ab\x1b[A\x1b[?2026;1$y\x1b[200~paste\x1b[201~\x1b[<4;12;3M\x1b[<64;12;3M");

    assert!(matches!(
        &events[0],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers,
            kind: KeyEventKind::Press,
        }) if modifiers.is_empty()
    ));
    assert!(matches!(
        &events[1],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('b'),
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        &events[2],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Up,
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        &events[3],
        TerminalEvent::Response(TerminalResponse::Decrpm { mode: 2026, status })
            if *status == DecrpmStatus::Set.code()
    ));
    assert!(matches!(&events[4], TerminalEvent::Paste(text) if text == "paste"));
    assert!(matches!(
        &events[5],
        TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
            modifiers,
            column: 11,
            row: 2,
            kind: MouseEventKind::Down(MouseButton::Left),
            ..
        }) if modifiers.contains(KeyModifiers::SHIFT)
    ));
    assert!(matches!(
        &events[6],
        TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
            column: 11,
            row: 2,
            kind: MouseEventKind::ScrollUp,
            ..
        })
    ));

    assert!(parser.flush_events().is_empty());

    let text_events = terminal_parsed_input_to_events(TerminalParsedInput::Key(
        parse_terminal_input_event("typed"),
    ));
    assert_eq!(text_events.len(), 5);
    assert!(matches!(
        &text_events[4],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('d'),
            ..
        })
    ));

    let escape_events = terminal_parsed_input_to_events(TerminalParsedInput::Key(
        parse_terminal_input_event("\x1b"),
    ));
    assert!(matches!(
        escape_events.as_slice(),
        [TerminalEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers,
            ..
        })] if modifiers.is_empty()
    ));
}

#[test]
fn test_terminal_raw_input_mode_sequences_are_opt_in_and_reversible() {
    let options = TerminalRawInputModeOptions {
        hide_cursor: true,
        bracketed_paste: true,
        focus_events: true,
        mouse_capture: false,
        keyboard_enhancement_flags: None,
        xterm_modify_other_keys: false,
    };

    let mut enter = Vec::new();
    write_terminal_raw_input_mode_enter(&mut enter, options).unwrap();
    let enter = String::from_utf8(enter).unwrap();
    assert!(
        enter.contains("\x1b[?25l"),
        "enter should hide cursor: {enter:?}"
    );
    assert!(
        enter.contains("\x1b[?1004h"),
        "enter should enable focus events: {enter:?}"
    );
    assert!(
        enter.contains("\x1b[?2004h"),
        "enter should enable bracketed paste: {enter:?}"
    );
    assert!(
        !enter.contains("\x1b[?1000h"),
        "mouse capture is opt-in: {enter:?}"
    );
    assert!(
        !enter.contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
        "modifyOtherKeys is opt-in: {enter:?}"
    );

    let mut exit = Vec::new();
    write_terminal_raw_input_mode_exit(&mut exit, options).unwrap();
    let exit = String::from_utf8(exit).unwrap();
    assert!(
        exit.contains("\x1b[?2004l"),
        "exit should disable bracketed paste: {exit:?}"
    );
    assert!(
        exit.contains("\x1b[?1004l"),
        "exit should disable focus events: {exit:?}"
    );
    assert!(
        exit.contains("\x1b[?25h"),
        "exit should show cursor: {exit:?}"
    );
}

#[test]
fn test_terminal_raw_input_mode_can_enable_tmux_compatible_extended_keys() {
    let options = TerminalRawInputModeOptions {
        hide_cursor: false,
        bracketed_paste: false,
        focus_events: false,
        mouse_capture: false,
        keyboard_enhancement_flags: Some(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        xterm_modify_other_keys: true,
    };

    let mut enter = Vec::new();
    write_terminal_raw_input_mode_enter(&mut enter, options).unwrap();
    let enter = String::from_utf8(enter).unwrap();
    assert!(
        enter.contains("\x1b[>1u"),
        "enter should push Kitty disambiguate flag: {enter:?}"
    );
    assert!(
        enter.contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
        "enter should enable xterm modifyOtherKeys: {enter:?}"
    );

    let mut exit = Vec::new();
    write_terminal_raw_input_mode_exit(&mut exit, options).unwrap();
    let exit = String::from_utf8(exit).unwrap();
    assert!(
        exit.contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
        "exit should reset xterm modifyOtherKeys: {exit:?}"
    );
    assert!(
        exit.contains("\x1b[<1u"),
        "exit should pop Kitty keyboard flags: {exit:?}"
    );
}

#[test]
fn test_terminal_raw_input_mode_guard_cleans_up_on_drop_and_explicit_exit() {
    let options = TerminalRawInputModeOptions {
        hide_cursor: true,
        bracketed_paste: true,
        focus_events: false,
        mouse_capture: false,
        keyboard_enhancement_flags: None,
        xterm_modify_other_keys: true,
    };

    let mut dropped = Vec::new();
    {
        let mut guard = TerminalRawInputModeGuard::enter(&mut dropped, options).unwrap();
        assert!(guard.is_active());
        guard.writer_mut().write_all(b"payload").unwrap();
    }
    let dropped = String::from_utf8(dropped).unwrap();
    let payload = dropped
        .find("payload")
        .unwrap_or_else(|| panic!("expected payload between enter/exit sequences: {dropped:?}"));
    assert!(
        dropped[..payload].contains("\x1b[?25l"),
        "drop guard should write enter sequence before payload: {dropped:?}"
    );
    assert!(
        dropped[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
        "drop guard should enable modifyOtherKeys before payload: {dropped:?}"
    );
    assert!(
        dropped[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
        "drop guard should reset modifyOtherKeys after payload: {dropped:?}"
    );
    assert!(
        dropped.ends_with("\x1b[?25h"),
        "drop guard should restore cursor visibility last: {dropped:?}"
    );

    let options = TerminalRawInputModeOptions {
        hide_cursor: false,
        bracketed_paste: false,
        focus_events: true,
        mouse_capture: false,
        keyboard_enhancement_flags: None,
        xterm_modify_other_keys: false,
    };
    let mut guard = TerminalRawInputModeGuard::enter(Vec::new(), options).unwrap();
    guard.writer_mut().write_all(b"explicit").unwrap();
    let explicit = String::from_utf8(guard.exit().unwrap()).unwrap();
    assert!(
        explicit.contains("\x1b[?1004h"),
        "explicit guard should write enter focus sequence: {explicit:?}"
    );
    assert!(
        explicit.contains("explicit"),
        "payload should be preserved: {explicit:?}"
    );
    assert!(
        explicit.contains("\x1b[?1004l"),
        "explicit guard should write exit focus sequence: {explicit:?}"
    );
}

#[test]
fn test_terminal_raw_input_session_guard_keeps_os_raw_mode_opt_in() {
    let terminal_modes = TerminalRawInputModeOptions {
        hide_cursor: true,
        bracketed_paste: true,
        focus_events: true,
        mouse_capture: false,
        keyboard_enhancement_flags: None,
        xterm_modify_other_keys: true,
    };
    let options = TerminalRawInputSessionOptions {
        terminal_modes,
        enable_os_raw_mode: false,
    };

    let mut guard = TerminalRawInputSessionGuard::enter(Vec::new(), options).unwrap();
    assert!(guard.is_active());
    assert!(!guard.is_os_raw_mode_enabled());
    assert_eq!(guard.options(), options);
    guard.writer_mut().write_all(b"session-payload").unwrap();

    let output = String::from_utf8(guard.exit().unwrap()).unwrap();
    let payload = output
        .find("session-payload")
        .unwrap_or_else(|| panic!("expected payload between enter/exit sequences: {output:?}"));
    assert!(
        output[..payload].contains("\x1b[?25l"),
        "session should hide the cursor before payload: {output:?}"
    );
    assert!(
        output[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
        "session should enable explicit modifyOtherKeys before payload: {output:?}"
    );
    assert!(
        output[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
        "session should reset explicit modifyOtherKeys after payload: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?25h"),
        "session should restore cursor visibility last: {output:?}"
    );

    let mut dropped = Vec::new();
    {
        let mut guard = TerminalRawInputSessionGuard::enter(&mut dropped, options).unwrap();
        guard.writer_mut().write_all(b"drop-session").unwrap();
    }
    let dropped = String::from_utf8(dropped).unwrap();
    let payload = dropped.find("drop-session").unwrap_or_else(|| {
        panic!("expected drop payload between enter/exit sequences: {dropped:?}")
    });
    assert!(
        dropped[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
        "drop session should enable modifyOtherKeys before payload: {dropped:?}"
    );
    assert!(
        dropped[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
        "drop session should reset modifyOtherKeys after payload: {dropped:?}"
    );
}

#[test]
fn test_terminal_raw_input_session_event_stream_scopes_modes_and_reader() {
    let terminal_modes = TerminalRawInputModeOptions {
        hide_cursor: true,
        bracketed_paste: true,
        focus_events: false,
        mouse_capture: false,
        keyboard_enhancement_flags: None,
        xterm_modify_other_keys: true,
    };
    let options = TerminalRawInputSessionOptions {
        terminal_modes,
        enable_os_raw_mode: false,
    };
    let reader = futures::io::Cursor::new(b"ab\x1b[A".to_vec());

    let mut session = TerminalRawInputSessionEventStream::from_reader_with_chunk_size(
        Vec::new(),
        reader,
        2,
        options,
    )
    .unwrap();
    assert!(session.guard().is_active());
    assert!(!session.guard().is_os_raw_mode_enabled());
    session
        .guard_mut()
        .writer_mut()
        .write_all(b"session-ui")
        .unwrap();

    let events = smol::block_on(async {
        let mut events = Vec::new();
        while let Some(event) = session.next().await {
            events.push(event.unwrap());
        }
        events
    });
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        &events[1],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('b'),
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        &events[2],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Up,
            kind: KeyEventKind::Press,
            ..
        })
    ));

    let output = String::from_utf8(session.exit().unwrap()).unwrap();
    let payload = output.find("session-ui").unwrap_or_else(|| {
        panic!("expected session payload between enter/exit sequences: {output:?}")
    });
    assert!(
        output[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
        "session stream should enter modes before payload: {output:?}"
    );
    assert!(
        output[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
        "session stream should exit modes after parsing reader: {output:?}"
    );
}

#[test]
fn test_terminal_raw_input_frontend_bridges_bytes_events_and_flush_timing() {
    let mut frontend = TerminalRawInputFrontend::new();

    let output = frontend.feed_bytes(b"a\x1b[?2026;1$y\x1b[200~paste\x1b[201~\x1b[<64;12;3M");
    assert_eq!(output.pending_flush_timeout, None);
    assert!(matches!(
        output.parsed.as_slice(),
        [
            TerminalParsedInput::Key(event),
            TerminalParsedInput::Response(TerminalResponse::Decrpm { mode: 2026, status }),
            TerminalParsedInput::Paste(text),
            TerminalParsedInput::Key(wheel),
        ] if event.input == "a"
            && *status == DecrpmStatus::Set.code()
            && text == "paste"
            && wheel.key.wheel_up
    ));
    assert!(matches!(
        &output.events[0],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        &output.events[1],
        TerminalEvent::Response(TerminalResponse::Decrpm { mode: 2026, .. })
    ));
    assert!(matches!(&output.events[2], TerminalEvent::Paste(text) if text == "paste"));
    assert!(matches!(
        &output.events[3],
        TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 11,
            row: 2,
            ..
        })
    ));

    let output = frontend.feed("\x1b[");
    assert!(output.parsed.is_empty());
    assert_eq!(
        output.pending_flush_timeout,
        Some(TERMINAL_INPUT_NORMAL_TIMEOUT)
    );
    assert!(frontend.has_incomplete_sequence());
    assert!(frontend.flush_if_due(true).is_none());
    let flushed = frontend
        .flush_if_due(false)
        .expect("expired timer with no queued input should flush");
    assert_eq!(flushed.pending_flush_timeout, None);
    assert!(matches!(
        flushed.parsed.as_slice(),
        [TerminalParsedInput::Key(event)] if event.input == "[?" || event.input == "["
    ));
}

#[test]
fn test_terminal_raw_input_event_stream_adapts_byte_chunks_and_flushes_on_end() {
    let source = stream::iter(vec![
        b"a\x1b[".to_vec(),
        b"A\x1b[200~paste".to_vec(),
        b"\x1b[201~".to_vec(),
        b"\x1b".to_vec(),
    ]);
    let events: Vec<_> = smol::block_on(TerminalRawInputEventStream::new(source).collect());

    assert!(matches!(
        &events[0],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        &events[1],
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Up,
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(&events[2], TerminalEvent::Paste(text) if text == "paste"));
    assert!(matches!(
        events.last(),
        Some(TerminalEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            kind: KeyEventKind::Press,
            ..
        }))
    ));
}

#[test]
fn test_terminal_raw_input_fallible_reader_stream_adapts_async_read() {
    let reader = futures::io::Cursor::new(b"ab\x1b[A".to_vec());
    let events = smol::block_on(
        TerminalRawInputFallibleEventStream::from_reader_with_chunk_size(reader, 2)
            .collect::<Vec<_>>(),
    );
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events[0].as_ref().unwrap(),
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        events[1].as_ref().unwrap(),
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Char('b'),
            kind: KeyEventKind::Press,
            ..
        })
    ));
    assert!(matches!(
        events[2].as_ref().unwrap(),
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Up,
            kind: KeyEventKind::Press,
            ..
        })
    ));

    let chunks = smol::block_on(
        TerminalRawInputByteStream::with_chunk_size(
            futures::io::Cursor::new(b"abcdef".to_vec()),
            3,
        )
        .collect::<Vec<_>>(),
    );
    assert_eq!(
        chunks
            .into_iter()
            .map(|chunk| chunk.unwrap())
            .collect::<Vec<_>>(),
        vec![b"abc".to_vec(), b"def".to_vec()]
    );
}

#[test]
fn test_terminal_query_sequences_and_response_parser_match_cc_ink() {
    assert_eq!(decrqm_query_sequence(2026), "\x1b[?2026$p");
    assert_eq!(da1_query_sequence(), "\x1b[c");
    assert_eq!(da2_query_sequence(), "\x1b[>c");
    assert_eq!(kitty_keyboard_query_sequence(), "\x1b[?u");
    assert_eq!(cursor_position_query_sequence(), "\x1b[?6n");
    assert_eq!(
        osc_color_query_sequence_with_env(11, |_| None),
        "\x1b]11;?\x07"
    );
    assert_eq!(
        osc_color_query_sequence_with_env(11, |key| {
            (key == "TERM").then(|| "xterm-kitty".to_string())
        }),
        "\x1b]11;?\x1b\\"
    );

    let decrpm = parse_terminal_response("\x1b[?2026;1$y").unwrap();
    assert_eq!(
        decrpm,
        TerminalResponse::Decrpm {
            mode: 2026,
            status: DecrpmStatus::Set.code(),
        }
    );
    assert_eq!(decrpm.decrpm_status(), Some(DecrpmStatus::Set));
    assert_eq!(DecrpmStatus::from_code(99), None);
    assert_eq!(
        parse_terminal_response("\x1b[?1;2c"),
        Some(TerminalResponse::Da1 { params: vec![1, 2] })
    );
    assert_eq!(
        parse_terminal_response("\x1b[>0;95;0c"),
        Some(TerminalResponse::Da2 {
            params: vec![0, 95, 0]
        })
    );
    assert_eq!(
        parse_terminal_response("\x1b[?7u"),
        Some(TerminalResponse::KittyKeyboard { flags: 7 })
    );
    assert_eq!(
        parse_terminal_response("\x1b[?24;80R"),
        Some(TerminalResponse::CursorPosition { row: 24, col: 80 })
    );
    assert_eq!(
        parse_terminal_response("\x1b]11;rgb:0000/0000/0000\x1b\\"),
        Some(TerminalResponse::Osc {
            code: 11,
            data: "rgb:0000/0000/0000".to_string()
        })
    );
    assert_eq!(
        parse_terminal_response("\x1bP>|xterm.js(5.5.0)\x1b\\"),
        Some(TerminalResponse::Xtversion {
            name: "xterm.js(5.5.0)".to_string()
        })
    );
    assert_eq!(parse_terminal_response("\x1b[1;2R"), None);
}

#[test]
fn test_terminal_response_parser_buffers_chunked_responses() {
    let mut parser = TerminalResponseParser::new();
    assert!(parser.feed("typed text \x1bP>|xterm").is_empty());
    assert_eq!(parser.buffered(), "\x1bP>|xterm");
    assert_eq!(
        parser.feed(".js(5.5.0)\x1b\\ trailing"),
        vec![TerminalResponse::Xtversion {
            name: "xterm.js(5.5.0)".to_string()
        }]
    );
    assert_eq!(parser.buffered(), "");
}

#[test]
fn test_terminal_response_parser_scans_mixed_csi_osc_and_ignores_keys() {
    let mut parser = TerminalResponseParser::new();
    let events =
        parser.feed_events("\x1b[A\x1b[?2026;1$yplain\x1b]11;rgb:0000/0000/0000\x07\x1b[?24;80R");
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        TerminalEvent::Response(TerminalResponse::Decrpm {
            mode: 2026,
            status: 1
        })
    ));
    assert!(matches!(
        &events[1],
        TerminalEvent::Response(TerminalResponse::Osc { code: 11, data })
            if data == "rgb:0000/0000/0000"
    ));
    assert!(matches!(
        &events[2],
        TerminalEvent::Response(TerminalResponse::CursorPosition { row: 24, col: 80 })
    ));
}

#[test]
fn test_terminal_response_parser_buffers_incomplete_osc_st() {
    let mut parser = TerminalResponseParser::new();
    assert!(parser.feed("\x1b]11;rgb:ffff").is_empty());
    assert_eq!(parser.buffered(), "\x1b]11;rgb:ffff");
    assert_eq!(
        parser.feed("/eeee/dddd\x1b\\"),
        vec![TerminalResponse::Osc {
            code: 11,
            data: "rgb:ffff/eeee/dddd".to_string()
        }]
    );
}

#[test]
fn test_terminal_response_parser_skips_non_response_string_sequences() {
    let mut parser = TerminalResponseParser::new();
    assert_eq!(
        parser.feed("\x1b^ignore \x1b[?2026;1$y\x1b\\\x1b[?2026;2$y"),
        vec![TerminalResponse::Decrpm {
            mode: 2026,
            status: 2,
        }]
    );
}

#[test]
fn test_terminal_querier_matches_cc_flush_barrier_semantics() {
    let mut querier = TerminalQuerier::new(Vec::new());
    let decrpm = querier.send(TerminalQuery::decrqm(2026)).unwrap();
    let flush = querier.flush().unwrap();
    assert_eq!(
        String::from_utf8(querier.output_ref().clone()).unwrap(),
        "\x1b[?2026$p\x1b[c"
    );

    let response = TerminalResponse::Decrpm {
        mode: 2026,
        status: 1,
    };
    assert!(querier.on_event(&TerminalEvent::Response(response.clone())));
    assert_eq!(futures::executor::block_on(decrpm), Some(response));

    querier.on_response(TerminalResponse::Da1 { params: vec![1, 2] });
    futures::executor::block_on(flush);

    let unsupported = querier.send(TerminalQuery::decrqm(2027)).unwrap();
    let flush = querier.flush().unwrap();
    querier.on_response(TerminalResponse::Da1 { params: vec![1] });
    assert_eq!(futures::executor::block_on(unsupported), None);
    futures::executor::block_on(flush);
}

#[test]
fn test_terminal_querier_explicit_da1_query_precedes_flush_sentinel() {
    let mut querier = TerminalQuerier::new(Vec::new());
    let da1 = querier.send(TerminalQuery::da1()).unwrap();
    let flush = querier.flush().unwrap();

    let first = TerminalResponse::Da1 { params: vec![1] };
    querier.on_response(first.clone());
    assert_eq!(futures::executor::block_on(da1), Some(first));

    querier.on_response(TerminalResponse::Da1 { params: vec![2] });
    futures::executor::block_on(flush);
}

#[test]
fn test_terminal_query_methods_write_side_band_and_resolve() {
    let (dest, buf) = new_test_writer();
    let mut term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();

    // Discard startup cursor-hide output; terminal queries should write raw
    // side-band control sequences without entering synchronized update.
    buf.lock().unwrap().clear();

    let pending = term
        .send_terminal_query(TerminalQuery::xtversion())
        .unwrap();
    let flush = term.flush_terminal_queries().unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert_eq!(output, "\x1b[>0q\x1b[c");
    assert!(
        !output.contains("\x1b[?2026"),
        "terminal queries are non-visual side-band controls: {output:?}"
    );

    let response = TerminalResponse::Xtversion {
        name: "xterm.js(5.5.0)".to_string(),
    };
    term.on_terminal_response(response.clone());
    assert_eq!(futures::executor::block_on(pending), Some(response));

    term.on_terminal_response(TerminalResponse::Da1 { params: vec![1] });
    futures::executor::block_on(flush);
}

#[test]
fn test_terminal_query_starts_event_stream_without_input_subscriber() {
    let (mut term, _output) = Terminal::mock(MockTerminalConfig::default());
    assert!(term.is_raw_mode_supported());
    assert!(!term.is_raw_mode_enabled());

    let pending = term.send_terminal_query(TerminalQuery::da1()).unwrap();

    assert!(
        term.is_raw_mode_enabled(),
        "query response routing should activate the backend event stream even without use_input/use_terminal_events"
    );
    term.on_terminal_response(TerminalResponse::Da1 { params: vec![1, 2] });
    assert_eq!(
        futures::executor::block_on(pending),
        Some(TerminalResponse::Da1 { params: vec![1, 2] })
    );
}

mod fullscreen;
mod log_update;

/// Changing keyboard enhancement flags while enhancement is active must swap the
/// flags in place (pop + push); while inactive it must only update the stored value.
#[test]
fn test_set_keyboard_enhancement_flags() {
    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term(dest, 0);

    // Inactive: no escape output, but the new flags are stored.
    let flags = event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
    term.set_keyboard_enhancement_flags(flags).unwrap();
    assert_eq!(term.keyboard_enhancement_flags, flags);
    assert!(buf.lock().unwrap().is_empty(), "no output while inactive");

    // Active: swapping flags emits pop + push.
    term.enabled_keyboard_enhancement = true;
    let new_flags = event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
    term.set_keyboard_enhancement_flags(new_flags).unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[<1u"),
        "expected pop sequence: {output:?}"
    );
    assert!(
        output.contains("\x1b[>2u"),
        "expected push of REPORT_EVENT_TYPES: {output:?}"
    );

    // Setting identical flags is a no-op.
    buf.lock().unwrap().clear();
    term.set_keyboard_enhancement_flags(new_flags).unwrap();
    assert!(buf.lock().unwrap().is_empty(), "same flags must be a no-op");
}

#[test]
fn test_keyboard_enhancement_reassertion_pops_before_push() {
    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term(dest, 0);
    term.enabled_keyboard_enhancement = true;
    term.keyboard_enhancement_flags = event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES;

    term.reassert_keyboard_enhancement().unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let pop = output
        .find("\x1b[<1u")
        .unwrap_or_else(|| panic!("expected pop sequence before reassert push: {output:?}"));
    let push = output
        .find("\x1b[>2u")
        .unwrap_or_else(|| panic!("expected push sequence after reassert pop: {output:?}"));
    assert!(
        pop < push,
        "keyboard reassertion should pop before push to keep Kitty stack depth balanced: {output:?}"
    );
}

#[test]
fn test_stdin_resume_reasserts_keyboard_and_mouse_without_focus_clear() {
    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, 0);
    term.raw_mode_enabled = true;
    term.enabled_keyboard_enhancement = true;
    term.mouse_capture = true;

    term.reassert_after_stdin_resume().unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let pop = output
        .find("\x1b[<1u")
        .unwrap_or_else(|| panic!("expected keyboard pop before stdin-gap reassert: {output:?}"));
    let push = output
        .find("\x1b[>2u")
        .unwrap_or_else(|| panic!("expected keyboard push after stdin-gap reassert: {output:?}"));
    assert!(
        pop < push,
        "stdin-gap reassert should keep Kitty stack depth balanced: {output:?}"
    );
    assert!(
        output.contains("\x1b[?1006h"),
        "stdin-gap reassert should restore mouse tracking: {output:?}"
    );
    assert!(
        !output.contains("\x1b[?1004h"),
        "stdin-gap reassert mirrors CC Ink and avoids extra focus-reporting writes: {output:?}"
    );
}

#[test]
fn test_resize_reasserts_mouse_capture_when_raw_mode_enabled() {
    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, 0);
    term.raw_mode_enabled = true;
    term.mouse_capture = true;

    term.reassert_after_resize().unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[?1004h"),
        "resize self-heal should re-enable focus reporting: {output:?}"
    );
    assert!(
        output.contains("\x1b[?1006h"),
        "resize self-heal should re-enable SGR mouse tracking: {output:?}"
    );

    buf.lock().unwrap().clear();
    term.mouse_capture = false;
    term.reassert_after_resize().unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[?1004h"),
        "resize self-heal should still re-enable focus reporting: {output:?}"
    );
    assert!(
        !output.contains("\x1b[?1006h"),
        "resize self-heal should not emit mouse tracking when capture is disabled: {output:?}"
    );
}

#[test]
fn test_resize_event_triggers_terminal_mode_reassertion() {
    let reasserts = Arc::new(AtomicUsize::new(0));
    let inner = ResizeReassertTerminal {
        events: Some(stream::iter(vec![TerminalEvent::Resize(120, 40)]).boxed()),
        reasserts: reasserts.clone(),
        size: None,
        dest: io::sink(),
    };
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: false,
    };

    term.start_event_stream().unwrap();
    smol::block_on(term.wait());

    assert_eq!(term.size(), Some((120, 40)));
    assert_eq!(
        reasserts.load(Ordering::SeqCst),
        1,
        "resize events should re-assert terminal modes before returning to render"
    );
}

#[test]
fn test_stdin_gap_reasserts_terminal_modes_like_cc_ink() {
    let reasserts = Arc::new(AtomicUsize::new(0));
    let inner = ResizeReassertTerminal {
        events: Some(
            stream::iter(vec![TerminalEvent::Key(KeyEvent::new(
                KeyEventKind::Press,
                KeyCode::Char('a'),
            ))])
            .boxed(),
        ),
        reasserts: reasserts.clone(),
        size: None,
        dest: io::sink(),
    };
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now() - STDIN_RESUME_GAP - Duration::from_secs(1)),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: false,
    };

    term.start_event_stream().unwrap();
    smol::block_on(term.wait());

    assert_eq!(
        reasserts.load(Ordering::SeqCst),
        1,
        "stdin silence longer than CC Ink's resume gap should re-assert terminal modes"
    );
}

#[test]
fn test_same_size_resize_event_is_ignored() {
    let reasserts = Arc::new(AtomicUsize::new(0));
    let inner = ResizeReassertTerminal {
        events: Some(
            stream::iter(vec![
                TerminalEvent::Resize(80, 24),
                TerminalEvent::Resize(120, 40),
            ])
            .boxed(),
        ),
        reasserts: reasserts.clone(),
        size: Some((80, 24)),
        dest: io::sink(),
    };
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: false,
    };

    term.start_event_stream().unwrap();
    smol::block_on(term.wait());

    assert_eq!(term.size(), Some((120, 40)));
    assert_eq!(
        reasserts.load(Ordering::SeqCst),
        1,
        "same-dimension resize events should not reassert modes or wake render"
    );
}

#[test]
fn test_synchronized_update_is_lazy() {
    let (dest, buf) = new_test_writer();
    let mut term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    term.synchronized_update_supported = true;

    // StdTerminal::new hides the cursor; discard setup bytes.
    buf.lock().unwrap().clear();

    term.synchronized_update(|_| Ok(())).unwrap();
    assert!(
        buf.lock().unwrap().is_empty(),
        "empty synchronized updates must not emit BSU/ESU"
    );

    term.synchronized_update(|term| {
        term.write_all(b"hello")?;
        Ok(())
    })
    .unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.starts_with("\x1b[?2026h"),
        "first real write should begin synchronized update: {output:?}"
    );
    assert!(
        output.contains("hello"),
        "payload should be written: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?2026l"),
        "drop should end synchronized update after writes: {output:?}"
    );
}

#[test]
fn test_synchronized_update_stays_lazy_for_noop_cursor_positioning() {
    let (dest, buf) = new_test_writer();
    let mut term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    term.synchronized_update_supported = true;

    // StdTerminal::new hides the cursor; discard setup bytes.
    buf.lock().unwrap().clear();

    term.synchronized_update(|term| term.position_cursor(None))
        .unwrap();
    assert!(
        buf.lock().unwrap().is_empty(),
        "no-op cursor positioning must preserve the empty synchronized-update fast path"
    );
}

#[test]
fn test_synchronized_update_stays_lazy_for_noop_clear_canvas() {
    let (dest, buf) = new_test_writer();
    let mut term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    term.synchronized_update_supported = true;

    // StdTerminal::new hides the cursor; discard setup bytes.
    buf.lock().unwrap().clear();

    term.synchronized_update(|term| term.clear_canvas())
        .unwrap();
    assert!(
        buf.lock().unwrap().is_empty(),
        "clear_canvas with no retained output must preserve the empty synchronized-update fast path"
    );
}

#[test]
fn test_synchronized_update_wraps_clear_canvas_when_it_writes() {
    let (dest, buf) = new_test_writer();
    let inner = new_inline_term(dest, 1);
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: true,
    };

    term.synchronized_update(|term| term.clear_canvas())
        .unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.starts_with("\x1b[?2026h"),
        "real clear_canvas should start synchronized update: {output:?}"
    );
    assert!(
        output.contains("\x1b[J"),
        "real clear_canvas payload should be present: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?2026l"),
        "real clear_canvas should end synchronized update: {output:?}"
    );
}

#[test]
fn test_synchronized_update_stays_lazy_for_identical_canvas_write() {
    let mut canvas = Canvas::new(10, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, "hello", CanvasTextStyle::default());

    let (dest, buf) = new_test_writer();
    let mut inner = new_inline_term_with_size(dest, canvas.height() as _, (10, 5));
    inner.prev_size_on_write = inner.size;
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: true,
    };

    term.synchronized_update(|term| term.write_canvas(Some(&canvas), &canvas))
        .unwrap();
    assert!(
        buf.lock().unwrap().is_empty(),
        "identical canvas writes must preserve the empty synchronized-update fast path"
    );
}

#[test]
fn test_synchronized_update_wraps_current_damage_canvas_write() {
    let mut prev = Canvas::new(10, 1);
    prev.subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    let mut next = prev.clone();
    next.mark_damage(crate::canvas::DamageRegion {
        x: 0,
        y: 0,
        width: 10,
        height: 1,
    });

    let (dest, buf) = new_test_writer();
    let mut inner = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
    inner.prev_size_on_write = inner.size;
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: true,
    };

    term.synchronized_update(|term| term.write_canvas(Some(&prev), &next))
        .unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.starts_with("\x1b[?2026h"),
        "current damage should start synchronized update: {output:?}"
    );
    assert!(
        output.contains("hello"),
        "damaged identical row should be repainted: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?2026l"),
        "current damage should end synchronized update: {output:?}"
    );
}

#[test]
fn test_synchronized_update_wraps_previous_damage_canvas_write() {
    let mut prev = Canvas::new(10, 1);
    prev.subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    prev.mark_damage(crate::canvas::DamageRegion {
        x: 0,
        y: 0,
        width: 10,
        height: 1,
    });
    let mut next = prev.clone();
    next.clear_damage();

    let (dest, buf) = new_test_writer();
    let mut inner = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
    inner.prev_size_on_write = inner.size;
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: true,
    };

    term.synchronized_update(|term| term.write_canvas(Some(&prev), &next))
        .unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.starts_with("\x1b[?2026h"),
        "previous damage should start synchronized update: {output:?}"
    );
    assert!(
        output.contains("hello"),
        "previous damaged identical row should be repainted: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?2026l"),
        "previous damage should end synchronized update: {output:?}"
    );
}

#[test]
fn test_synchronized_update_stays_lazy_for_identical_fullscreen_canvas_write() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());

    let (dest, buf) = new_test_writer();
    let mut inner = new_fullscreen_term(dest, 0, canvas.height() as _);
    inner.size = Some((10, 2));
    inner.prev_size_on_write = Some((10, 2));
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: true,
    };

    term.synchronized_update(|term| term.write_canvas(Some(&canvas), &canvas))
        .unwrap();
    assert!(
        buf.lock().unwrap().is_empty(),
        "identical fullscreen canvas writes must preserve the empty synchronized-update fast path"
    );
}

#[test]
fn test_synchronized_update_wraps_cursor_positioning_when_it_writes() {
    use crate::canvas::CursorDeclaration;
    let (dest, buf) = new_test_writer();
    let mut term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    term.synchronized_update_supported = true;

    // StdTerminal::new hides the cursor; discard setup bytes.
    buf.lock().unwrap().clear();

    term.synchronized_update(|term| {
        term.position_cursor(Some(CursorDeclaration {
            x: 1,
            y: 0,
            visible: false,
        }))
    })
    .unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.starts_with("\x1b[?2026h"),
        "cursor movement should start synchronized update: {output:?}"
    );
    assert!(
        output.contains("\x1b[2G"),
        "cursor movement payload should be present: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?2026l"),
        "cursor movement should end synchronized update: {output:?}"
    );
}

#[test]
fn test_synchronized_update_wraps_pending_wrap_resolution() {
    let (dest, buf) = new_test_writer();
    let mut inner = new_inline_term(dest, 1);
    inner.inline_pending_wrap = true;
    let mut term = Terminal {
        inner: Box::new(inner),
        output: Output::Stdout,
        base_fullscreen: false,
        event_stream: None,
        subscribers: Vec::new(),
        event_cell_snapshot: None,
        terminal_focus_state: None,
        last_stdin_event_at: Some(Instant::now()),
        query_queue: VecDeque::new(),
        pending_ctrl_c: Vec::new(),
        received_ctrl_c: false,
        ignore_ctrl_c: false,
        suspend_on_ctrl_z: false,
        synchronized_update_depth: 0,
        synchronized_update_started: false,
        synchronized_update_supported: true,
    };

    term.synchronized_update(|term| term.position_cursor(None))
        .unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.starts_with("\x1b[?2026h"),
        "pending-wrap CR should start synchronized update: {output:?}"
    );
    assert!(
        output.contains('\r'),
        "pending-wrap resolution should emit CR: {output:?}"
    );
    assert!(
        output.ends_with("\x1b[?2026l"),
        "pending-wrap CR should end synchronized update: {output:?}"
    );
}

#[test]
fn test_synchronized_update_skips_markers_when_unsupported() {
    let (dest, buf) = new_test_writer();
    let mut term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();
    term.synchronized_update_supported = false;

    // StdTerminal::new hides the cursor; discard setup bytes.
    buf.lock().unwrap().clear();

    term.synchronized_update(|term| {
        term.write_all(b"hello")?;
        Ok(())
    })
    .unwrap();
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert_eq!(
        output, "hello",
        "unsupported terminals should receive payload without BSU/ESU"
    );
}

/// The panic-restore registry must track live terminals and reset the fullscreen
/// count as fullscreen terminals are dropped, so the hook does not leave the
/// alternate screen when only inline terminals remain alive.
#[test]
fn test_panic_restore_registration_counts() {
    // Snapshot the baseline: other tests may have live terminals.
    let (baseline_live, baseline_fullscreen) = {
        let state = PANIC_RESTORE_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.live_terminals, state.fullscreen_terminals)
    };

    register_terminal_for_panic_restore(true);
    register_terminal_for_panic_restore(false);
    {
        let state = PANIC_RESTORE_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(state.live_terminals, baseline_live + 2);
        assert_eq!(state.fullscreen_terminals, baseline_fullscreen + 1);
    }

    unregister_terminal_for_panic_restore(true);
    {
        let state = PANIC_RESTORE_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(state.live_terminals, baseline_live + 1);
        assert_eq!(state.fullscreen_terminals, baseline_fullscreen);
    }

    unregister_terminal_for_panic_restore(false);
    if baseline_live == 0 {
        let state = PANIC_RESTORE_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(state.live_terminals, 0);
        assert_eq!(state.fullscreen_terminals, 0);
    }
}

/// After a resume from suspension, the terminal must forget everything it knew
/// about previously written output, so the next write_canvas behaves like a
/// first write (full render at the current cursor position) instead of a row
/// diff against content that the shell has since overwritten.
#[test]
fn test_fullscreen_terminal_enters_alt_screen_clears_and_homes() {
    let (dest, buf) = new_test_writer();
    let _term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        true,
        false,
    )
    .unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[?1049h"),
        "fullscreen startup should enter alternate screen: {output:?}"
    );
    assert!(
        output.contains("\x1b[2J"),
        "fullscreen startup should clear stale alternate-screen cells: {output:?}"
    );
    assert!(
        output.contains("\x1b[1;1H"),
        "fullscreen startup should home the first frame anchor: {output:?}"
    );
}

#[test]
fn test_dynamic_alternate_screen_enters_clears_homes_and_exits() {
    let (dest, buf) = new_test_writer();
    let mut term = Terminal::new(
        Box::new(dest),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
    )
    .unwrap();

    buf.lock().unwrap().clear();
    let changed = term
        .set_dynamic_alternate_screen(Some(crate::context::AlternateScreenRequest {
            mouse_tracking: false,
        }))
        .unwrap();
    assert!(changed);
    assert!(term.is_fullscreen());
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[?1049h"),
        "dynamic AlternateScreen should enter alt screen: {output:?}"
    );
    assert!(
        output.contains("\x1b[2J"),
        "dynamic AlternateScreen should clear stale alt-screen cells: {output:?}"
    );
    assert!(
        output.contains("\x1b[1;1H"),
        "dynamic AlternateScreen should home the first frame anchor: {output:?}"
    );

    buf.lock().unwrap().clear();
    let changed = term.set_dynamic_alternate_screen(None).unwrap();
    assert!(changed);
    assert!(!term.is_fullscreen());
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[?1049l"),
        "unmounting AlternateScreen should leave alt screen: {output:?}"
    );
}

#[test]
fn test_reinitialize_after_resume_resets_output_state() {
    let (dest, _buf) = new_test_writer();
    let mut term = new_inline_term(dest, 5);
    term.prev_size_on_write = Some((10, 10));
    term.prev_canvas_top_row = 3;

    term.reinitialize_after_resume().unwrap();

    assert_eq!(term.prev_canvas_height, 0);
    assert_eq!(term.prev_canvas_top_row, 0);
    assert_eq!(term.prev_size_on_write, None);
}

#[test]
fn test_fullscreen_reinitialize_after_resume_reenters_clears_and_homes() {
    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 7, 4);
    term.prev_size_on_write = Some((80, 24));
    term.inline_pending_wrap = true;
    term.mouse_capture = true;

    term.reinitialize_after_resume().unwrap();

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("\x1b[?1049h"),
        "fullscreen resume should re-enter alternate screen: {output:?}"
    );
    assert!(
        output.contains("\x1b[2J"),
        "fullscreen resume should erase stale alternate-screen cells: {output:?}"
    );
    assert!(
        output.contains("\x1b[1;1H"),
        "fullscreen resume should home the cursor before repaint: {output:?}"
    );
    let home = output
        .find("\x1b[1;1H")
        .unwrap_or_else(|| panic!("expected cursor home: {output:?}"));
    let mouse = output
        .rfind("\x1b[?1006h")
        .unwrap_or_else(|| panic!("expected SGR mouse tracking reassertion: {output:?}"));
    assert!(
        mouse > home,
        "fullscreen resume should re-enable mouse tracking after alt-screen re-entry/home: {output:?}"
    );
    assert_eq!(term.prev_canvas_height, 0);
    assert_eq!(term.prev_canvas_top_row, 0);
    assert_eq!(term.prev_size_on_write, None);
    assert!(!term.inline_pending_wrap);
}

#[test]
fn test_inline_diff_unchanged_row_skipped() {
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "first", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "second", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "first", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "changed", CanvasTextStyle::default());

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term(dest, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    // Build vt: render prev, then apply diff output.
    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "first     ");
    assert_eq!(vt.line(1).text(), "changed   ");
    assert_eq!(vt.cursor().row, 1);
}

#[test]
fn test_inline_diff_rewrites_from_first_changed_column() {
    let style = CanvasTextStyle::default();
    let mut prev = Canvas::new(12, 1);
    prev.subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "prefix-old", style);
    let mut next = Canvas::new(12, 1);
    next.subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "prefix-new", style);

    let (diff, vt) = inline_diff_vt(&prev, &next, (12, 5));
    let diff = String::from_utf8(diff).unwrap();
    assert!(
        diff.contains("\x1b[8Gnew"),
        "inline diff should jump to the first changed column: {diff:?}"
    );
    assert!(
        !diff.contains("prefix-"),
        "inline diff should not repaint the unchanged prefix: {diff:?}"
    );
    assert_eq!(vt.line(0).text(), "prefix-new  ");
}

#[test]
fn test_inline_first_diff_after_initial_write_full_rewrites_once() {
    let style = CanvasTextStyle::default();
    let mut first = Canvas::new(10, 2);
    first
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "same", style);
    first
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "first", style);

    let mut second = Canvas::new(10, 2);
    second
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "same", style);
    second
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "second", style);

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, 0, (10, 6));
    term.write_canvas(None, &first).unwrap();
    assert!(
        term.inline_force_full_rewrite_next_diff,
        "first inline frame should mark the next diff as contaminated"
    );

    diff_buf.lock().unwrap().clear();
    term.write_canvas(Some(&first), &second).unwrap();
    let first_diff = String::from_utf8(diff_buf.lock().unwrap().clone()).unwrap();
    assert!(
        first_diff.contains("\x1b[J"),
        "contaminated first diff should clear the previous inline canvas: {first_diff:?}"
    );
    assert!(
        first_diff.contains("same") && first_diff.contains("second"),
        "contaminated first diff should rewrite the whole canvas, not just changed rows: {first_diff:?}"
    );
    assert!(
        !term.inline_force_full_rewrite_next_diff,
        "contaminated-frame guard should be one-shot"
    );

    let mut third = Canvas::new(10, 2);
    third
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "same", style);
    third
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "third", style);

    diff_buf.lock().unwrap().clear();
    term.write_canvas(Some(&second), &third).unwrap();
    let second_diff = String::from_utf8(diff_buf.lock().unwrap().clone()).unwrap();
    assert!(
        !second_diff.contains("\x1b[J"),
        "subsequent clean diff should return to sparse row updates: {second_diff:?}"
    );
    assert!(second_diff.contains("third"));
    assert!(
        !second_diff.contains("same"),
        "unchanged rows should be skipped after the one-shot contaminated rewrite: {second_diff:?}"
    );
}

#[test]
fn test_inline_diff_shrinking() {
    let mut prev = Canvas::new(10, 3);
    prev.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "bbb", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "ccc", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "ddd", CanvasTextStyle::default());

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term(dest, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "aaa       ");
    assert_eq!(vt.line(1).text(), "ddd       ");
    assert_eq!(
        vt.line(2).text(),
        "          ",
        "old row 2 should be cleared"
    );
    assert_eq!(vt.cursor().row, 1, "cursor on last row of new canvas");
}

#[test]
fn test_inline_diff_growing() {
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "bbb", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 3);
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "bbb", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "ccc", CanvasTextStyle::default());

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term(dest, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "aaa       ");
    assert_eq!(vt.line(1).text(), "bbb       ");
    assert_eq!(vt.line(2).text(), "ccc       ");
    assert_eq!(vt.cursor().row, 2, "cursor on last row of new canvas");
}

#[test]
fn test_inline_diff_non_adjacent_rows_forward() {
    // Two non-adjacent rows change within the existing canvas. The diff
    // visits row 1 first (moving the cursor up from row 4), then row 3
    // (moving forward but still within the old canvas). This exercises the
    // Greater branch when y < prev_height.
    let style = CanvasTextStyle::default();

    let mut prev = Canvas::new(10, 5);
    for i in 0..5 {
        prev.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, i, &format!("row{i}"), style);
    }

    let mut next = Canvas::new(10, 5);
    for i in 0..5 {
        next.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, i, &format!("row{i}"), style);
    }
    // Use same-length replacements to avoid masking the bug with
    // trailing-cell issues in write_ansi_row_without_newline.
    next.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 1, "AAA1", style);
    next.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 3, "BBB3", style);

    let (_diff, vt) = inline_diff_vt(&prev, &next, (10, 10));

    assert_eq!(vt.line(0).text(), "row0      ");
    assert_eq!(vt.line(1).text(), "AAA1      ");
    assert_eq!(vt.line(2).text(), "row2      ");
    assert_eq!(vt.line(3).text(), "BBB3      ");
    assert_eq!(vt.line(4).text(), "row4      ");
}

#[test]
fn test_inline_diff_growing_at_bottom_of_screen() {
    // Simulate the canvas being at the bottom of the terminal so that
    // growing from 1 row to 2 requires scrolling. MoveToNextLine (CSI E)
    // won't create new lines at the screen bottom — only \r\n will.
    let style = CanvasTextStyle::default();

    let mut prev = Canvas::new(10, 1);
    prev.subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, "hello", style);

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", style);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", style);

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term(dest, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    // Fill the VT so the canvas starts on the last row, then apply the diff.
    let mut setup = Vec::new();
    let vt_rows = 5;
    for i in 0..vt_rows - 1 {
        write!(setup, "line{i}\r\n").unwrap();
    }
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, vt_rows);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    // The VT should have scrolled: line0 is gone, canvas occupies last 2 rows.
    assert_eq!(vt.line(vt_rows - 2).text(), "hello     ");
    assert_eq!(vt.line(vt_rows - 1).text(), "world     ");
    assert_eq!(
        vt.cursor().row,
        vt_rows - 1,
        "cursor on last row of new canvas"
    );
}

#[test]
fn test_inline_diff_identical_canvas_is_noop() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term(dest, canvas.height() as _);
    term.prev_size_on_write = term.size;
    term.write_canvas(Some(&canvas), &canvas).unwrap();

    assert!(
        diff_buf.lock().unwrap().is_empty(),
        "identical canvas should produce no output"
    );
}

#[test]
fn test_inline_prev_damage_region_rewrites_identical_reachable_row() {
    let mut prev = Canvas::new(10, 3);
    for (y, text) in ["one", "two", "three"].iter().enumerate() {
        prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
            0,
            y as isize,
            text,
            CanvasTextStyle::default(),
        );
    }
    prev.mark_damage(crate::canvas::DamageRegion {
        x: 0,
        y: 1,
        width: 10,
        height: 1,
    });

    let next = prev.clone();
    let mut next_without_damage = next.clone();
    next_without_damage.clear_damage();

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
    term.prev_size_on_write = term.size;
    term.write_canvas(Some(&prev), &next_without_damage)
        .unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        diff.contains("two"),
        "previous-frame damage should be unioned into the next diff: {diff:?}"
    );
    assert!(
        !diff.contains("one") && !diff.contains("three"),
        "undamaged rows should remain skipped: {diff:?}"
    );
}

#[test]
fn test_inline_damage_region_rewrites_identical_reachable_row() {
    let mut prev = Canvas::new(10, 3);
    for (y, text) in ["one", "two", "three"].iter().enumerate() {
        prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
            0,
            y as isize,
            text,
            CanvasTextStyle::default(),
        );
    }

    let mut next = prev.clone();
    next.mark_damage(crate::canvas::DamageRegion {
        x: 0,
        y: 1,
        width: 10,
        height: 1,
    });

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
    term.prev_size_on_write = term.size;
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        diff.contains("two"),
        "damaged identical row should be repainted: {diff:?}"
    );
    assert!(
        !diff.contains("one") && !diff.contains("three"),
        "undamaged identical rows should remain skipped: {diff:?}"
    );
}

#[test]
fn test_inline_force_full_repaint_rewrites_identical_canvas() {
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());

    let mut next = prev.clone();
    next.force_full_repaint();

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
    term.prev_size_on_write = term.size;
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        !diff.contains("\x1b[J"),
        "reachable forced repaint should rewrite in place without clearing: {diff:?}"
    );
    assert!(
        diff.contains("hello") && diff.contains("world"),
        "forced full repaint should rewrite unchanged rows: {diff:?}"
    );

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());
    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());
    assert_eq!(vt.line(0).text(), "hello     ");
    assert_eq!(vt.line(1).text(), "world     ");
    assert_eq!(vt.cursor().row, 1);
}

#[test]
fn test_inline_force_full_repaint_rewrites_reachable_rows_without_scrollback_clear() {
    let mut prev = Canvas::new(10, 5);
    for (y, text) in ["one", "two", "three", "four", "five"].iter().enumerate() {
        prev.subview_mut(0, 0, 0, 0, 10, 5).set_text(
            0,
            y as isize,
            text,
            CanvasTextStyle::default(),
        );
    }

    let mut next = prev.clone();
    next.force_full_repaint();

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
    term.prev_size_on_write = term.size;
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        !diff.contains("\x1b[2J") && !diff.contains("\x1b[3J"),
        "forced repaint should not clear or purge native scrollback when protected rows are unchanged: {diff:?}"
    );
    assert!(
        !diff.contains("one"),
        "the cursor-restore-scroll protected top row should be skipped rather than duplicated into scrollback: {diff:?}"
    );
    assert!(
        diff.contains("two") && diff.contains("five"),
        "reachable rows should still be repainted: {diff:?}"
    );
}

#[test]
fn test_inline_force_full_repaint_growing_rewrites_without_clear() {
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 3);
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "world", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "again", CanvasTextStyle::default());
    next.force_full_repaint();

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
    term.prev_size_on_write = term.size;
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        !diff.contains("\x1b[J"),
        "growing forced repaint should append/rewrite in place without clearing: {diff:?}"
    );
    assert!(diff.contains("hello") && diff.contains("world") && diff.contains("again"));

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());
    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());
    assert_eq!(vt.line(0).text(), "hello     ");
    assert_eq!(vt.line(1).text(), "world     ");
    assert_eq!(vt.line(2).text(), "again     ");
    assert_eq!(vt.cursor().row, 2);
}

#[test]
fn test_inline_resize_forces_full_rewrite_even_when_canvas_identical() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term_with_size(dest, canvas.height() as _, (10, 5));
    term.prev_size_on_write = Some((12, 5));
    term.write_canvas(Some(&canvas), &canvas).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        diff.contains("\x1b[J"),
        "resize reset should clear the old inline canvas: {diff:?}"
    );
    assert!(
        diff.contains("hello") && diff.contains("world"),
        "resize reset should repaint the full canvas: {diff:?}"
    );
}

#[test]
fn test_fullscreen_diff_identical_canvas_is_noop() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, canvas.height() as _);
    term.write_canvas(Some(&canvas), &canvas).unwrap();

    let buf = diff_buf.lock().unwrap();
    assert!(
        buf.is_empty(),
        "identical fullscreen canvas should preserve the empty-diff zero-write fast path"
    );
}

#[test]
fn test_fullscreen_prev_damage_region_rewrites_identical_rows() {
    let mut prev = Canvas::new(10, 3);
    for (y, text) in ["one", "two", "three"].iter().enumerate() {
        prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
            0,
            y as isize,
            text,
            CanvasTextStyle::default(),
        );
    }
    prev.mark_damage(crate::canvas::DamageRegion {
        x: 0,
        y: 1,
        width: 10,
        height: 1,
    });

    let mut next = prev.clone();
    next.clear_damage();

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        diff.contains("two"),
        "previous-frame damage should be unioned into fullscreen diff: {diff:?}"
    );
    assert!(
        !diff.contains("one") && !diff.contains("three"),
        "undamaged fullscreen rows should remain skipped: {diff:?}"
    );
}

#[test]
fn test_fullscreen_damage_region_rewrites_identical_rows() {
    let mut prev = Canvas::new(10, 3);
    for (y, text) in ["one", "two", "three"].iter().enumerate() {
        prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
            0,
            y as isize,
            text,
            CanvasTextStyle::default(),
        );
    }

    let mut next = prev.clone();
    next.mark_damage(crate::canvas::DamageRegion {
        x: 0,
        y: 1,
        width: 10,
        height: 1,
    });

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        diff.contains("two"),
        "damaged identical fullscreen row should be repainted: {diff:?}"
    );
    assert!(
        !diff.contains("one") && !diff.contains("three"),
        "undamaged identical fullscreen rows should remain skipped: {diff:?}"
    );
}

#[test]
fn test_fullscreen_force_full_repaint_rewrites_identical_rows() {
    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());

    let mut next = prev.clone();
    next.force_full_repaint();

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
    assert!(
        diff.contains("hello") && diff.contains("world"),
        "forced full repaint should rewrite unchanged fullscreen rows: {diff:?}"
    );
}

#[test]
fn test_inline_diff_styled_text_preserved() {
    let bold_style = CanvasTextStyle {
        weight: Weight::Bold,
        color: Some(Color::Red),
        ..Default::default()
    };

    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", bold_style);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "old", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "hello", bold_style);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "new", bold_style);

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_inline_term(dest, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    // Row 0 unchanged: bold red "hello"
    let row0 = vt.line(0);
    assert_eq!(row0.text(), "hello     ");
    assert!(row0.cells()[0].pen().is_bold());
    assert!(row0.cells()[0].pen().foreground().is_some());

    // Row 1 updated: bold red "new"
    let row1 = vt.line(1);
    assert_eq!(row1.text(), "new       ");
    assert!(row1.cells()[0].pen().is_bold());
    assert!(row1.cells()[0].pen().foreground().is_some());
}

#[test]
fn test_fullscreen_diff_styled_text_preserved() {
    let underline_style = CanvasTextStyle {
        underline: true,
        color: Some(Color::Green),
        ..Default::default()
    };

    let mut prev = Canvas::new(10, 2);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "keep", underline_style);
    prev.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "old", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "keep", underline_style);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "new", underline_style);

    let (dest, diff_buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&diff_buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    // Row 0 unchanged
    let row0 = vt.line(0);
    assert_eq!(row0.text(), "keep      ");
    assert!(row0.cells()[0].pen().is_underline());

    // Row 1 updated with underline green
    let row1 = vt.line(1);
    assert_eq!(row1.text(), "new       ");
    assert!(row1.cells()[0].pen().is_underline());
    assert!(row1.cells()[0].pen().foreground().is_some());
}

#[test]
fn test_inline_diff_at_terminal_height_boundary() {
    // Canvas height == terminal height uses the normal diff path when only
    // visible rows changed (no off-screen changes trigger a fallback).
    let mut prev = Canvas::new(10, 5);
    prev.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 4, "bbb", CanvasTextStyle::default());

    let mut next = Canvas::new(10, 5);
    next.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 4, "ccc", CanvasTextStyle::default());

    let (_diff, vt) = inline_diff_vt(&prev, &next, (10, 5));

    assert_eq!(vt.line(0).text(), "aaa       ");
    assert_eq!(vt.line(4).text(), "ccc       ");
}

#[test]
fn test_inline_diff_tall_canvas_visible_change() {
    // Canvas (8 rows) taller than terminal (5 rows). Only the last row
    // changes, which is in the visible area — the normal diff path should
    // handle it without a full clear+rewrite.
    let style = CanvasTextStyle::default();

    let mut prev = Canvas::new(10, 8);
    for i in 0..8 {
        prev.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, i, &format!("row{i}"), style);
    }

    let mut next = Canvas::new(10, 8);
    for i in 0..7 {
        next.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, i, &format!("row{i}"), style);
    }
    next.subview_mut(0, 0, 0, 0, 10, 8)
        .set_text(0, 7, "CHANGED", style);

    let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));

    // Should NOT contain a full clear (ClearAll = ESC[2J)
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        !diff_str.contains("\x1b[2J"),
        "expected row-level diff, not full clear; got: {diff_str:?}"
    );

    // The bottom 5 rows of the 8-row canvas are visible in the terminal.
    assert_eq!(vt.line(0).text(), "row3      ");
    assert_eq!(vt.line(4).text(), "CHANGED   ");
}

#[test]
fn test_inline_diff_full_viewport_top_row_change_full_rewrite() {
    // Mirrors Ink/log-update's cursorRestoreScroll guard. When the previous
    // frame exactly filled the viewport, a cursor restore can push the top
    // row into scrollback; changing that row should therefore reset instead
    // of relying on a relative move to the viewport top.
    let style = CanvasTextStyle::default();
    let mut prev = Canvas::new(10, 5);
    for i in 0..5 {
        prev.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, i, &format!("row{i}"), style);
    }

    let mut next = prev.clone();
    next.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 0, "TOP", style);

    let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.contains("\x1b[2J"),
        "expected full reset for top row after full-viewport frame: {diff_str:?}"
    );
    assert_eq!(vt.line(0).text(), "TOP0      ");
    assert_eq!(vt.line(4).text(), "row4      ");
}

#[test]
fn test_inline_diff_tall_canvas_top_visible_change_full_rewrite() {
    // For an overflowing previous frame, Ink/log-update treats viewportY+1
    // rows as unreachable: the top visible row may have been pushed into
    // scrollback by cursor restoration. A change at row 3 for an 8-row
    // canvas in a 5-row viewport should therefore force a full reset.
    let style = CanvasTextStyle::default();

    let mut prev = Canvas::new(10, 8);
    for i in 0..8 {
        prev.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, i, &format!("row{i}"), style);
    }

    let mut next = prev.clone();
    next.subview_mut(0, 0, 0, 0, 10, 8)
        .set_text(0, 3, "TOPVIS", style);

    let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.contains("\x1b[2J"),
        "expected full reset for cursorRestoreScroll top visible row: {diff_str:?}"
    );
    assert_eq!(vt.line(0).text(), "TOPVIS    ");
    assert_eq!(vt.line(4).text(), "row7      ");
}

#[test]
fn test_inline_diff_tall_canvas_offscreen_change() {
    // Canvas (8 rows) taller than terminal (5 rows). A row above the
    // visible area changes — this must trigger the full-rewrite fallback
    // since we can't cursor to an off-screen row.
    let style = CanvasTextStyle::default();

    let mut prev = Canvas::new(10, 8);
    for i in 0..8 {
        prev.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, i, &format!("row{i}"), style);
    }

    let mut next = Canvas::new(10, 8);
    for i in 0..8 {
        next.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, i, &format!("row{i}"), style);
    }
    // Change row 1, which is above the visible area (visible_start = 8-5 = 3).
    next.subview_mut(0, 0, 0, 0, 10, 8)
        .set_text(0, 1, "OFFSCR", style);

    let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));

    // Should contain a full clear (ClearAll = ESC[2J, because
    // prev_canvas_height >= term_height triggers the heavy clear path).
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.contains("\x1b[2J"),
        "expected full clear fallback; got: {diff_str:?}"
    );

    // After full rewrite, the bottom 5 rows of the new canvas are visible.
    assert_eq!(vt.line(0).text(), "row3      ");
    assert_eq!(vt.line(4).text(), "row7      ");
}

#[test]
fn test_inline_diff_shrinking_from_scrollback_to_viewport_full_rewrite() {
    // Mirrors Ink/log-update's shrink->fits guard: when the previous
    // inline canvas overflowed the terminal, the visible terminal contains
    // the suffix of that canvas. Shrinking to a viewport-sized canvas should
    // reveal row0 again, but cursor/erase operations cannot pull rows out of
    // scrollback. Force a clear+repaint instead.
    let style = CanvasTextStyle::default();

    let mut prev = Canvas::new(10, 8);
    for i in 0..8 {
        prev.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, i, &format!("old{i}"), style);
    }

    let mut next = Canvas::new(10, 5);
    for i in 0..5 {
        next.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, i, &format!("new{i}"), style);
    }

    let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
    let diff_str = String::from_utf8_lossy(&diff);
    assert!(
        diff_str.contains("\x1b[2J"),
        "expected full clear fallback for shrink->fits; got: {diff_str:?}"
    );
    assert_eq!(vt.line(0).text(), "new0      ");
    assert_eq!(vt.line(4).text(), "new4      ");
    assert_eq!(vt.cursor().row, 4);
}

#[test]
fn test_inline_diff_sequential_updates() {
    let style = CanvasTextStyle::default();

    let mut c1 = Canvas::new(10, 2);
    c1.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaa", style);
    c1.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "bbb", style);

    let mut c2 = Canvas::new(10, 2);
    c2.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaa", style);
    c2.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "ccc", style);

    let mut c3 = Canvas::new(10, 3);
    c3.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "xxx", style);
    c3.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "ccc", style);
    c3.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "ddd", style);

    let (dest, buf) = new_test_writer();
    let mut term = new_inline_term(dest, c1.height() as _);

    // First diff: c1 -> c2
    term.write_canvas(Some(&c1), &c2).unwrap();
    // Second diff: c2 -> c3
    term.write_canvas(Some(&c2), &c3).unwrap();

    let mut setup = Vec::new();
    c1.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 6);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "xxx       ");
    assert_eq!(vt.line(1).text(), "ccc       ");
    assert_eq!(vt.line(2).text(), "ddd       ");
    assert_eq!(vt.cursor().row, 2, "cursor on last row of final canvas");
}

#[test]
fn test_fullscreen_diff_sequential_updates() {
    let style = CanvasTextStyle::default();

    let mut c1 = Canvas::new(10, 2);
    c1.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaa", style);
    c1.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "bbb", style);

    let mut c2 = Canvas::new(10, 2);
    c2.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "aaa", style);
    c2.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "ccc", style);

    let mut c3 = Canvas::new(10, 3);
    c3.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 0, "xxx", style);
    c3.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "ccc", style);
    c3.subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 2, "ddd", style);

    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, c1.height() as _);

    term.write_canvas(Some(&c1), &c2).unwrap();
    term.write_canvas(Some(&c2), &c3).unwrap();

    let mut setup = Vec::new();
    c1.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&buf.lock().unwrap());

    let mut vt = avt::Vt::new(10, 6);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "xxx       ");
    assert_eq!(vt.line(1).text(), "ccc       ");
    assert_eq!(vt.line(2).text(), "ddd       ");
    assert_eq!(vt.cursor().row, 2, "cursor on last row of final canvas");
}

#[test]
fn test_fullscreen_diff_rewrites_from_first_changed_column() {
    let style = CanvasTextStyle::default();
    let mut prev = Canvas::new(12, 1);
    prev.subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "prefix-old", style);
    let mut next = Canvas::new(12, 1);
    next.subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "prefix-new", style);

    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
    term.size = Some((12, 5));
    term.prev_size_on_write = Some((12, 5));
    term.write_canvas(Some(&prev), &next).unwrap();

    let diff = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        diff.contains("\x1b[1;8Hnew"),
        "fullscreen diff should jump to the first changed column: {diff:?}"
    );
    assert!(
        !diff.contains("prefix-"),
        "fullscreen diff should not repaint the unchanged prefix: {diff:?}"
    );

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(diff.as_bytes());
    let mut vt = avt::Vt::new(12, 5);
    vt.feed_str(&String::from_utf8(setup).unwrap());
    assert_eq!(vt.line(0).text(), "prefix-new  ");
}

#[test]
fn test_borrowed_writers() {
    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();

    {
        let mut terminal = Terminal::new(
            Box::new(&mut stdout_buf),
            Box::new(&mut stderr_buf),
            Output::Stdout,
            false,
            true,
        )
        .unwrap();
        let canvas = Canvas::new(10, 1);
        terminal.write_canvas(None, &canvas).unwrap();
    }

    assert!(!stdout_buf.is_empty());
}

/// Helper: build a pair of 10×5 canvases (4 content rows + 1 footer) that
/// differ only in a single cell's background color on `changed_row`,
/// simulating a mouse-highlight overlay.
fn make_fullscreen_diff_canvases(changed_row: usize) -> (Canvas, Canvas) {
    let style = CanvasTextStyle::default();
    let width = 10;
    let height = 5;

    let build = |highlight: bool| {
        let mut c = Canvas::new(width, height);
        let mut sv = c.subview_mut(0, 0, 0, 0, width, height);
        for y in 0..4u32 {
            sv.set_text(0, y as isize, &format!("row{y}"), style);
        }
        sv.set_text(0, 4, "FOOTER", style);
        sv.set_background_color(0, 4, width, 1, Color::Green);
        if highlight {
            sv.set_background_color(0, changed_row as isize, 1, 1, Color::Yellow);
        }
        c
    };

    (build(false), build(true))
}

/// Verify that with `prev_canvas_top_row = 0` the fullscreen row-level
/// diff writes each changed row to its correct terminal position.
///
/// Uses a layout with numbered content rows and a distinct footer, where
/// a single cell changes between frames (as a mouse-highlight overlay
/// would cause).
#[test]
fn test_fullscreen_diff_zero_top_row_renders_correctly() {
    let (prev, next) = make_fullscreen_diff_canvases(2);
    let width = prev.width();
    let height = prev.height();

    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 0, height as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    // Replay: write prev canvas as the baseline already on screen,
    // then apply the diff on top.
    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&buf.lock().unwrap());

    let mut vt = avt::Vt::new(width, height + 2);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    assert_eq!(vt.line(0).text(), "row0      ");
    assert_eq!(vt.line(1).text(), "row1      ");
    assert_eq!(vt.line(2).text(), "row2      ");
    assert_eq!(vt.line(3).text(), "row3      ");
    assert_eq!(
        vt.line(4).text(),
        "FOOTER    ",
        "every row must appear at its correct terminal position"
    );
}

/// Counterpart: with a non-zero `prev_canvas_top_row`, every changed row Y
/// is written to terminal line `top_row + Y` instead of line Y.  Unchanged
/// rows are skipped by `row_eq`, so the corruption is never self-correcting.
///
/// This demonstrates why `prev_canvas_top_row` must be anchored at 0 in
/// fullscreen mode — any stale cursor position causes the entire diff to
/// be offset.
#[test]
fn test_fullscreen_diff_nonzero_top_row_offsets_changed_rows() {
    let (prev, next) = make_fullscreen_diff_canvases(1);
    let width = prev.width();
    let height = prev.height();

    // With top_row = 2 (simulating a stale cursor value), the diff for
    // changed row 1 writes to terminal position 2+1 = 3 instead of 1.
    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 2, height as _);
    term.write_canvas(Some(&prev), &next).unwrap();

    let mut setup = Vec::new();
    prev.write_ansi_without_final_newline(&mut setup).unwrap();
    setup.extend_from_slice(&buf.lock().unwrap());

    let mut vt = avt::Vt::new(width, height + 4);
    vt.feed_str(&String::from_utf8(setup).unwrap());

    // Row 1's diff landed at terminal line 3 (offset 2+1) instead of 1,
    // overwriting row 3's original content.
    assert_eq!(
        vt.line(3).text(),
        "row1      ",
        "row 1's diff landed at terminal line 3 (offset 2+1) instead of line 1"
    );
}

/// Regression test: exercises the full initial-write → diff pipeline.
///
/// `write_canvas(None, …)` must set `prev_canvas_top_row` to 0.  This path
/// runs both on the very first frame and whenever `clear_terminal_output()`
/// triggers a full rewrite on a subsequent frame — in either case the
/// cursor may not be at (0, 0), so the code must reset it explicitly.
///
/// Without the fix, the old code called `cursor::position()` which returns
/// a stale value inside `BeginSynchronizedUpdate` on real terminals, and
/// fails outright in non-TTY test environments (timeout → panic).
#[test]
fn test_fullscreen_initial_write_sets_zero_top_row() {
    let (initial, next) = make_fullscreen_diff_canvases(2);
    let width = initial.width();
    let height = initial.height();

    let (dest, buf) = new_test_writer();
    let mut term = new_fullscreen_term(dest, 99, 0); // start with intentionally wrong value

    // The initial write must set prev_canvas_top_row = 0 (the fix).
    // Without the fix, this panics due to cursor::position() timeout.
    term.write_canvas(None, &initial).unwrap();
    assert_eq!(
        term.prev_canvas_top_row, 0,
        "initial fullscreen write must anchor prev_canvas_top_row at 0"
    );

    // Subsequent diff should render correctly with top_row = 0.
    term.write_canvas(Some(&initial), &next).unwrap();

    let mut vt = avt::Vt::new(width, height + 2);
    vt.feed_str(&String::from_utf8(buf.lock().unwrap().clone()).unwrap());

    assert_eq!(vt.line(0).text(), "row0      ");
    assert_eq!(vt.line(1).text(), "row1      ");
    assert_eq!(vt.line(2).text(), "row2      ");
    assert_eq!(vt.line(3).text(), "row3      ");
    assert_eq!(vt.line(4).text(), "FOOTER    ");
}
