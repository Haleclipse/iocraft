#![allow(unused_imports)]
use super::super::*;
use crate::prelude::*;
use crossterm::{csi, style::Colored};

#[test]
fn test_selection_state_click_without_drag_does_not_select() {
    let mut selection = SelectionState::new();
    selection.start(1, 0);
    selection.update(1, 0);
    assert!(!selection.has_selection());
    selection.update(2, 0);
    assert!(selection.has_selection());
    selection.finish();
    assert!(!selection.is_dragging());
}

#[test]
fn test_selection_click_tracker_matches_cc_ink_thresholds() {
    let mut tracker = SelectionClickTracker::new();

    assert_eq!(
        tracker.record_press(10, 5, 1_000),
        SelectionMousePressKind::Single
    );
    assert_eq!(
        tracker.record_press(11, 6, 1_499),
        SelectionMousePressKind::Double
    );
    assert_eq!(
        tracker.record_press(11, 6, 1_998),
        SelectionMousePressKind::Triple
    );
    assert_eq!(
        tracker.record_press(11, 6, 2_100),
        SelectionMousePressKind::Triple,
        "quadruple and later clicks are capped to line-selection semantics"
    );
}

#[test]
fn test_selection_click_tracker_resets_on_timeout_or_distance() {
    let mut tracker = SelectionClickTracker::new();

    assert_eq!(
        tracker.record_press(2, 2, 100),
        SelectionMousePressKind::Single
    );
    assert_eq!(
        tracker.record_press(2, 2, 600),
        SelectionMousePressKind::Single,
        "CC Ink uses a strict < 500ms threshold"
    );
    assert_eq!(
        tracker.record_press(2, 2, 700),
        SelectionMousePressKind::Double
    );
    assert_eq!(
        tracker.record_press(4, 2, 800),
        SelectionMousePressKind::Single,
        "movement beyond one cell breaks the multi-click chain"
    );
    tracker.reset();
    assert_eq!(
        tracker.record_press(4, 2, 801),
        SelectionMousePressKind::Single
    );
}

#[test]
fn test_selection_controller_single_press_drag_release_and_take_text() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    let outcome = controller.handle_left_press(&canvas, 1, 0, 1_000, true);
    assert_eq!(outcome.kind, SelectionMousePressKind::Single);
    assert!(!outcome.finished_previous_drag);
    assert!(!outcome.cancel_pending_hyperlink);
    assert!(controller.selection().last_press_had_alt());
    assert!(
        !controller.has_selection(),
        "bare press is not yet a selection"
    );

    controller.handle_drag(&canvas, 3, 0);
    assert!(controller.has_selection());
    assert_eq!(controller.selected_text(&canvas), "bcd");
    assert!(controller.handle_release());
    assert!(!controller.selection().is_dragging());

    assert_eq!(controller.take_selected_text(&canvas), "bcd");
    assert!(!controller.has_selection());
}

#[test]
fn test_selection_controller_copy_on_select_text_fires_once_after_release() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    controller.handle_drag(&canvas, 3, 0);
    assert!(!controller.copy_on_select_would_mutate());
    assert_eq!(controller.copy_on_select_text(&canvas), None);
    controller.handle_release();

    assert!(controller.copy_on_select_would_mutate());
    assert_eq!(
        controller.copy_on_select_text(&canvas).as_deref(),
        Some("bcd")
    );
    assert!(!controller.copy_on_select_would_mutate());
    assert_eq!(controller.copy_on_select_text(&canvas), None);
    assert!(
        controller.has_selection(),
        "copy-on-select should not clear highlight"
    );
}

#[test]
fn test_selection_controller_copy_on_select_text_resets_for_new_drag() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    controller.handle_drag(&canvas, 3, 0);
    controller.handle_release();
    assert_eq!(
        controller.copy_on_select_text(&canvas).as_deref(),
        Some("bcd")
    );

    controller.handle_left_press(&canvas, 2, 0, 2_000, false);
    controller.handle_drag(&canvas, 4, 0);
    controller.handle_release();
    assert_eq!(
        controller.copy_on_select_text(&canvas).as_deref(),
        Some("cde")
    );
}

#[test]
fn test_selection_controller_copy_on_select_text_noops_for_click_without_drag() {
    let canvas = Canvas::new(8, 1);
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    controller.handle_release();

    assert_eq!(controller.copy_on_select_text(&canvas), None);
}

#[test]
fn test_selection_controller_copy_on_select_skips_whitespace_only_once() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "a   b", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    controller.handle_drag(&canvas, 3, 0);
    controller.handle_release();

    assert!(controller.has_selection());
    assert_eq!(controller.copy_on_select_text(&canvas), None);
    assert_eq!(
        controller.copy_on_select_text(&canvas),
        None,
        "whitespace-only selection should settle the copy-on-select guard"
    );

    controller.handle_left_press(&canvas, 0, 0, 2_000, false);
    controller.handle_drag(&canvas, 4, 0);
    controller.handle_release();
    assert_eq!(
        controller.copy_on_select_text(&canvas).as_deref(),
        Some("a   b")
    );
}

#[test]
fn test_selection_controller_double_click_drag_extends_by_word() {
    let mut canvas = Canvas::new(16, 1);
    canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text(
        0,
        0,
        "one two three",
        CanvasTextStyle::default(),
    );
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 4, 0, 1_000, false);
    let outcome = controller.handle_left_press(&canvas, 4, 0, 1_200, false);
    assert_eq!(outcome.kind, SelectionMousePressKind::Double);
    assert!(outcome.cancel_pending_hyperlink);
    assert_eq!(controller.selected_text(&canvas), "two");

    controller.handle_drag(&canvas, 10, 0);
    assert_eq!(controller.selected_text(&canvas), "two three");
}

#[test]
fn test_selection_controller_non_left_press_resets_click_chain() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    assert_eq!(
        controller
            .handle_left_press(&canvas, 1, 0, 1_000, false)
            .kind,
        SelectionMousePressKind::Single
    );
    controller.handle_non_left_press();
    assert_eq!(
        controller
            .handle_left_press(&canvas, 1, 0, 1_100, false)
            .kind,
        SelectionMousePressKind::Single
    );
}

#[test]
fn test_selection_controller_no_button_motion_finishes_drag_and_dedupes_hover() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    controller.handle_drag(&canvas, 3, 0);
    assert!(controller.no_button_motion_would_change(3, 0));
    let first = controller.handle_no_button_motion(3, 0);
    assert!(first.finished_drag);
    assert_eq!(first.hover, Some(SelectionPoint { col: 3, row: 0 }));
    assert!(!controller.selection().is_dragging());

    assert!(!controller.no_button_motion_would_change(3, 0));
    let repeat = controller.handle_no_button_motion(3, 0);
    assert!(!repeat.finished_drag);
    assert_eq!(repeat.hover, None);
    assert!(controller.no_button_motion_would_change(4, 0));
    let moved = controller.handle_no_button_motion(4, 0);
    assert_eq!(moved.hover, Some(SelectionPoint { col: 4, row: 0 }));
}

#[test]
fn test_selection_controller_focus_loss_finishes_drag() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    assert!(controller.handle_focus_lost());
    assert!(!controller.selection().is_dragging());
    assert!(!controller.handle_focus_lost());
}

#[test]
fn test_selection_controller_finish_drag_resets_autoscroll_direction() {
    let mut canvas = Canvas::new(6, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 1);
    controller.selection_mut().update(3, 4);
    assert_eq!(
        controller.drag_scroll_direction(1, 3),
        Some(SelectionDragScrollDirection::Down)
    );
    controller.translate_for_drag_autoscroll(&canvas, SelectionDragScrollDirection::Down, 1, 1, 3);

    assert!(controller.handle_release());

    assert_eq!(controller.last_drag_scroll_dir, None);
}

#[test]
fn test_selection_controller_drag_scroll_direction_requires_anchor_in_viewport() {
    let mut canvas = Canvas::new(6, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 0, "head", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(0, 4);

    assert_eq!(controller.drag_scroll_direction(1, 3), None);
}

#[test]
fn test_selection_controller_drag_autoscroll_down_captures_above_and_shifts_anchor() {
    let mut canvas = Canvas::new(6, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 2, "row2", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 1);
    controller.selection_mut().update(3, 4);

    assert_eq!(
        controller.drag_scroll_direction(1, 3),
        Some(SelectionDragScrollDirection::Down)
    );
    let outcome = controller.translate_for_drag_autoscroll(
        &canvas,
        SelectionDragScrollDirection::Down,
        1,
        1,
        3,
    );

    assert!(outcome.translated);
    assert_eq!(
        controller.selection().anchor(),
        Some(SelectionPoint { col: 0, row: 1 })
    );
    assert_eq!(controller.selection.virtual_anchor_row, Some(0));
    assert_eq!(
        controller.selection.scrolled_off_above,
        vec!["row1".to_string()]
    );
}

#[test]
fn test_selection_controller_drag_autoscroll_up_captures_below_and_shifts_anchor() {
    let mut canvas = Canvas::new(6, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 2, "row2", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 3, "row3", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 3);
    controller.selection_mut().update(3, 0);

    assert_eq!(
        controller.drag_scroll_direction(1, 3),
        Some(SelectionDragScrollDirection::Up)
    );
    controller.translate_for_drag_autoscroll(&canvas, SelectionDragScrollDirection::Up, 1, 1, 3);

    assert_eq!(
        controller.selection().anchor(),
        Some(SelectionPoint { col: 5, row: 3 })
    );
    assert_eq!(controller.selection.virtual_anchor_row, Some(4));
    assert_eq!(
        controller.selection.scrolled_off_below,
        vec!["r".to_string()],
        "anchor-side column constraint is applied before capture then reset for future rows"
    );
}

#[test]
fn test_selection_controller_drag_scroll_blocked_reversal_clears_captures() {
    let mut canvas = Canvas::new(6, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 1);
    controller.selection_mut().update(3, 4);
    assert_eq!(
        controller.drag_scroll_direction(1, 3),
        Some(SelectionDragScrollDirection::Down)
    );
    controller.translate_for_drag_autoscroll(&canvas, SelectionDragScrollDirection::Down, 1, 1, 3);

    controller.selection_mut().update(3, 0);
    assert_eq!(controller.drag_scroll_direction(1, 3), None);
    assert!(controller.selection().captured_rows_empty());
    assert_eq!(controller.last_drag_scroll_dir, None);
}

#[test]
fn test_selection_controller_follow_scroll_drag_shifts_anchor_only() {
    let mut canvas = Canvas::new(6, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "row0", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "row2", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(3, 2);

    let outcome = controller.translate_for_follow_scroll(&canvas, 1, 0, 2);

    assert_eq!(
        outcome,
        SelectionScrollOutcome {
            translated: true,
            cleared: false,
        }
    );
    assert_eq!(
        controller.selection().anchor(),
        Some(SelectionPoint { col: 0, row: 0 })
    );
    assert_eq!(
        controller.selection().focus(),
        Some(SelectionPoint { col: 3, row: 2 })
    );
    assert_eq!(controller.selection.virtual_anchor_row, Some(-1));
    assert_eq!(
        controller.selection.scrolled_off_above,
        vec!["row0".to_string()]
    );
}

#[test]
fn test_selection_controller_follow_scroll_released_shifts_both_and_preserves_text() {
    let mut canvas = Canvas::new(6, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "row0", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "row2", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(3, 2);
    controller.selection_mut().finish();

    let outcome = controller.translate_for_follow_scroll(&canvas, 1, 0, 2);
    canvas.shift_rows(0, 2, 1);

    assert!(outcome.translated);
    assert!(!outcome.cleared);
    assert_eq!(controller.selected_text(&canvas), "row0\nrow1\nrow2");
}

#[test]
fn test_selection_controller_follow_scroll_clears_when_selection_leaves_top() {
    let mut canvas = Canvas::new(6, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "row0", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(3, 1);
    controller.selection_mut().finish();

    let outcome = controller.translate_for_follow_scroll(&canvas, 2, 0, 2);

    assert_eq!(
        outcome,
        SelectionScrollOutcome {
            translated: true,
            cleared: true,
        }
    );
    assert!(!controller.has_selection());
    assert!(controller.selection().captured_rows_empty());
}

#[test]
fn test_selection_controller_follow_scroll_ignores_static_focus_endpoint() {
    let mut canvas = Canvas::new(6, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 3, "foot", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 1);
    controller.selection_mut().update(3, 3);
    controller.selection_mut().finish();

    let outcome = controller.translate_for_follow_scroll(&canvas, 1, 1, 2);

    assert_eq!(outcome, SelectionScrollOutcome::default());
    assert_eq!(
        controller.selection().anchor(),
        Some(SelectionPoint { col: 0, row: 1 })
    );
    assert_eq!(
        controller.selection().focus(),
        Some(SelectionPoint { col: 3, row: 3 })
    );
}

#[test]
fn test_selection_controller_scroll_jump_down_preserves_copied_text() {
    let mut canvas = Canvas::new(6, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "row0", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "row2", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(3, 2);

    let outcome = controller.translate_for_scroll_jump(&canvas, 1, 0, 2);
    canvas.shift_rows(0, 2, 1);

    assert_eq!(
        outcome,
        SelectionScrollOutcome {
            translated: true,
            cleared: false,
        }
    );
    assert_eq!(controller.selected_text(&canvas), "row0\nrow1\nrow2");
}

#[test]
fn test_selection_controller_scroll_jump_up_preserves_copied_text() {
    let mut canvas = Canvas::new(6, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "row0", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "row2", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 0);
    controller.selection_mut().update(3, 2);

    let outcome = controller.translate_for_scroll_jump(&canvas, -1, 0, 2);
    canvas.shift_rows(0, 2, -1);

    assert!(outcome.translated);
    assert!(!outcome.cleared);
    assert_eq!(controller.selected_text(&canvas), "row0\nrow1\nrow2");
}

#[test]
fn test_selection_controller_scroll_jump_ignores_static_endpoint() {
    let mut canvas = Canvas::new(6, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 4)
        .set_text(0, 3, "foot", CanvasTextStyle::default());
    let mut controller = SelectionController::new();
    controller.selection_mut().start(0, 1);
    controller.selection_mut().update(3, 3);

    let outcome = controller.translate_for_scroll_jump(&canvas, 1, 1, 2);

    assert_eq!(outcome, SelectionScrollOutcome::default());
    assert_eq!(
        controller.selection().anchor(),
        Some(SelectionPoint { col: 0, row: 1 })
    );
    assert_eq!(
        controller.selection().focus(),
        Some(SelectionPoint { col: 3, row: 3 })
    );
}

#[test]
fn test_selection_controller_new_press_finishes_lost_release() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    controller.handle_drag(&canvas, 3, 0);
    let outcome = controller.handle_left_press(&canvas, 5, 0, 1_100, false);
    assert!(outcome.finished_previous_drag);
    assert_eq!(outcome.kind, SelectionMousePressKind::Single);
    assert_eq!(
        controller.selection().anchor(),
        Some(SelectionPoint { col: 5, row: 0 })
    );
    assert!(!controller.has_selection());
}

#[test]
fn test_selection_controller_release_classifies_single_click_and_link() {
    let mut canvas = Canvas::new(16, 1);
    canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text_with_link(
        0,
        0,
        "link",
        CanvasTextStyle::default(),
        Some("https://example.com"),
    );
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    let outcome = controller.handle_release_at(&canvas, 1, 0, false);

    assert!(outcome.was_dragging);
    assert_eq!(outcome.click, Some(SelectionPoint { col: 1, row: 0 }));
    assert_eq!(outcome.hyperlink.as_deref(), Some("https://example.com"));
    assert!(!controller.has_selection());
}

#[test]
fn test_selection_controller_release_suppresses_link_when_click_consumed_or_selected() {
    let mut canvas = Canvas::new(16, 1);
    canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text_with_link(
        0,
        0,
        "linktext",
        CanvasTextStyle::default(),
        Some("https://example.com"),
    );
    let mut controller = SelectionController::new();

    controller.handle_left_press(&canvas, 1, 0, 1_000, false);
    let consumed = controller.handle_release_at(&canvas, 1, 0, true);
    assert_eq!(consumed.click, Some(SelectionPoint { col: 1, row: 0 }));
    assert_eq!(consumed.hyperlink, None);

    controller.handle_left_press(&canvas, 1, 0, 2_000, false);
    controller.handle_drag(&canvas, 3, 0);
    let selected = controller.handle_release_at(&canvas, 3, 0, false);
    assert_eq!(selected.click, None);
    assert_eq!(selected.hyperlink, None);
    assert!(controller.has_selection());
}

#[test]
fn test_selection_state_tracks_alt_press_marker() {
    let mut selection = SelectionState::new();
    selection.start_with_alt(1, 0, true);
    assert!(selection.last_press_had_alt());
    selection.set_last_press_had_alt(false);
    assert!(!selection.last_press_had_alt());
    selection.clear();
    assert!(!selection.last_press_had_alt());
}

#[test]
fn test_selection_state_multi_click_falls_back_to_anchor_on_no_select() {
    let mut canvas = Canvas::new(8, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, " gutter", CanvasTextStyle::default());
    canvas.mark_no_select_region(0, 0, 3, 1);

    let mut selection = SelectionState::new();
    selection.start_multi_click(&canvas, 1, 0, SelectionClickCount::Double);

    assert!(selection.has_selection());
    assert_eq!(selection.anchor(), Some(SelectionPoint { col: 1, row: 0 }));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 1, row: 0 }));
    assert!(selection.anchor_span.is_none());
}

#[test]
fn test_selection_state_multi_click_selects_word_or_line() {
    let mut canvas = Canvas::new(12, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 12, 2)
        .set_text(0, 0, "one two", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 12, 2)
        .set_text(0, 1, "three", CanvasTextStyle::default());

    let mut selection = SelectionState::new();
    selection.start_multi_click(&canvas, 5, 0, SelectionClickCount::Double);
    assert_eq!(selection.selected_text(&canvas), "two");

    selection.start_multi_click(&canvas, 0, 1, SelectionClickCount::Triple);
    assert_eq!(selection.selected_text(&canvas), "three");
}

#[test]
fn test_selection_state_captures_scrolled_soft_wrap_rows() {
    let mut canvas = Canvas::new(6, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 1, "world", CanvasTextStyle::default());
    canvas.mark_soft_wrap_continuation(1, 6);

    let mut selection = SelectionState::new();
    selection.start(0, 0);
    selection.update(5, 1);
    selection.capture_scrolled_rows(&canvas, 0, 0, SelectionCaptureSide::Above);
    canvas.shift_rows(0, 1, 1);
    selection.shift_rows(-1, 0, 1, canvas.width());

    assert_eq!(
        selection.selected_text(&canvas),
        "hello world",
        "captured rows and shifted soft-wrap metadata should copy as one logical line"
    );
}

#[test]
fn test_selection_state_capture_resets_anchor_span_cols_above() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "one two", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "three", CanvasTextStyle::default());

    let mut selection = SelectionState::new();
    assert!(selection.select_word_at(&canvas, 4, 0));
    selection.extend_span_selection(&canvas, 2, 1);
    selection.capture_scrolled_rows(&canvas, 0, 0, SelectionCaptureSide::Above);

    let span = selection
        .anchor_span
        .expect("word span should remain active");
    assert_eq!(selection.anchor().unwrap().col, 0);
    assert_eq!(span.lo.col, 0);
    assert_eq!(span.hi.col, canvas.width() - 1);
}

#[test]
fn test_selection_state_capture_resets_anchor_span_cols_below() {
    let mut canvas = Canvas::new(10, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "one", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "two three", CanvasTextStyle::default());

    let mut selection = SelectionState::new();
    assert!(selection.select_word_at(&canvas, 4, 1));
    selection.extend_span_selection(&canvas, 1, 0);
    selection.capture_scrolled_rows(&canvas, 1, 1, SelectionCaptureSide::Below);

    let span = selection
        .anchor_span
        .expect("word span should remain active");
    assert_eq!(selection.anchor().unwrap().col, canvas.width() - 1);
    assert_eq!(span.lo.col, 0);
    assert_eq!(span.hi.col, canvas.width() - 1);
}

#[test]
fn test_selection_state_select_word_matches_terminal_classes() {
    let mut canvas = Canvas::new(24, 1);
    canvas.subview_mut(0, 0, 0, 0, 24, 1).set_text(
        0,
        0,
        "run /usr/bin/bash ok",
        CanvasTextStyle::default(),
    );

    let mut selection = SelectionState::new();
    assert!(selection.select_word_at(&canvas, 5, 0));
    assert_eq!(selection.selected_text(&canvas), "/usr/bin/bash");
}

#[test]
fn test_selection_state_select_word_steps_from_wide_tail() {
    let mut canvas = Canvas::new(6, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 1)
        .set_text(0, 0, "中a!", CanvasTextStyle::default());

    let mut selection = SelectionState::new();
    assert!(selection.select_word_at(&canvas, 1, 0));
    assert_eq!(selection.selected_text(&canvas), "中a");
}

#[test]
fn test_selection_state_select_line_uses_no_select_copy_filter() {
    let mut canvas = Canvas::new(10, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, " 42 code", CanvasTextStyle::default());
    canvas.mark_no_select_region(0, 0, 4, 1);

    let mut selection = SelectionState::new();
    assert!(selection.select_line_at(&canvas, 0));
    assert_eq!(selection.selected_text(&canvas), "code");
}

#[test]
fn test_selection_state_extends_word_span_forward_and_backward() {
    let mut canvas = Canvas::new(16, 1);
    canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text(
        0,
        0,
        "one two three",
        CanvasTextStyle::default(),
    );

    let mut selection = SelectionState::new();
    assert!(selection.select_word_at(&canvas, 4, 0));
    selection.extend_span_selection(&canvas, 10, 0);
    assert_eq!(selection.selected_text(&canvas), "two three");

    assert!(selection.select_word_at(&canvas, 4, 0));
    selection.extend_span_selection(&canvas, 1, 0);
    assert_eq!(selection.selected_text(&canvas), "one two");
}

#[test]
fn test_selection_state_keyboard_focus_moves_wrap_and_clamp() {
    let mut selection = SelectionState::new();
    selection.start(1, 1);
    selection.update(0, 1);

    assert!(selection.move_focus_by(SelectionFocusMove::Left, 4, 3));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 0 }));
    assert!(selection.move_focus_by(SelectionFocusMove::Right, 4, 3));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 0, row: 1 }));
    assert!(selection.move_focus_by(SelectionFocusMove::LineEnd, 4, 3));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 1 }));
    assert!(selection.move_focus_by(SelectionFocusMove::Down, 4, 3));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 2 }));
    assert!(!selection.move_focus_by(SelectionFocusMove::Down, 4, 3));
    assert!(selection.move_focus_by(SelectionFocusMove::Up, 4, 3));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 1 }));
    assert!(selection.move_focus_by(SelectionFocusMove::LineStart, 4, 3));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 0, row: 1 }));
}

#[test]
fn test_selection_state_keyboard_focus_drops_word_span() {
    let mut canvas = Canvas::new(12, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "one two", CanvasTextStyle::default());

    let mut selection = SelectionState::new();
    assert!(selection.select_word_at(&canvas, 4, 0));
    assert!(selection.anchor_span.is_some());
    assert!(selection.move_focus_by(SelectionFocusMove::Right, canvas.width(), canvas.height()));
    assert!(selection.anchor_span.is_none());
}

#[test]
fn test_selection_state_shift_rows_uses_virtual_rows_and_trims_capture_debt() {
    let mut canvas = Canvas::new(6, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "row0", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "row1", CanvasTextStyle::default());
    canvas
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "row2", CanvasTextStyle::default());

    let mut selection = SelectionState::new();
    selection.start(0, 0);
    selection.update(3, 2);
    selection.capture_scrolled_rows(&canvas, 0, 0, SelectionCaptureSide::Above);
    selection.shift_rows(-1, 0, 2, canvas.width());
    assert_eq!(selection.virtual_anchor_row, Some(-1));
    assert_eq!(selection.scrolled_off_above.len(), 1);

    selection.shift_rows(1, 0, 2, canvas.width());
    assert_eq!(selection.virtual_anchor_row, None);
    assert!(
        selection.scrolled_off_above.is_empty(),
        "reverse scroll should drop rows whose virtual debt returned on-screen"
    );
}

#[test]
fn test_selection_state_shift_anchor_and_follow_track_virtual_rows() {
    let mut selection = SelectionState::new();
    selection.start(1, 1);
    selection.update(2, 2);

    selection.shift_anchor(-2, 0, 2);
    assert_eq!(selection.anchor(), Some(SelectionPoint { col: 1, row: 0 }));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 2, row: 2 }));
    assert_eq!(selection.virtual_anchor_row, Some(-1));

    assert!(!selection.shift_for_follow(1, 0, 2));
    assert_eq!(selection.virtual_anchor_row, None);
    assert_eq!(selection.anchor(), Some(SelectionPoint { col: 1, row: 0 }));
    assert_eq!(selection.focus(), Some(SelectionPoint { col: 2, row: 2 }));
}

#[test]
fn test_selected_text_respects_no_select_and_wide_tails() {
    let mut canvas = Canvas::new(12, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 12, 2)
        .set_text(0, 0, " 42 +中x  ", CanvasTextStyle::default());
    canvas.mark_no_select_region(0, 0, 5, 1);

    let selected = canvas.selected_text(SelectionRange::new(
        SelectionPoint { col: 0, row: 0 },
        SelectionPoint { col: 9, row: 0 },
    ));

    assert_eq!(
        selected, "中x",
        "selection copy should skip noSelect gutter cells, wide-char tails, and trailing blanks"
    );
}

#[test]
fn test_apply_selection_overlay_skips_no_select_and_marks_damage() {
    let mut canvas = Canvas::new(6, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    canvas.mark_no_select_region(0, 0, 2, 1);

    let applied = canvas.apply_selection_overlay(
        SelectionRange::new(
            SelectionPoint { col: 0, row: 0 },
            SelectionPoint { col: 3, row: 0 },
        ),
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    );

    assert!(applied);

    assert!(!canvas.resolved_text_style(0, 0).unwrap().invert);
    assert!(!canvas.resolved_text_style(1, 0).unwrap().invert);
    assert!(canvas.resolved_text_style(2, 0).unwrap().invert);
    assert!(canvas.resolved_text_style(3, 0).unwrap().invert);
    assert_eq!(
        canvas.damage_region(),
        Some(DamageRegion {
            x: 2,
            y: 0,
            width: 2,
            height: 1,
        }),
        "selection overlay should damage only selectable overlaid cells"
    );
}

#[test]
fn test_selection_state_contains_and_apply_overlay() {
    let mut canvas = Canvas::new(6, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 6, 1)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    let mut selection = SelectionState::new();
    selection.start(1, 0);
    selection.update(3, 0);

    assert!(!selection.is_cell_selected(0, 0));
    assert!(selection.is_cell_selected(2, 0));
    assert!(selection.apply_overlay(
        &mut canvas,
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    ));
    assert!(!canvas.resolved_text_style(0, 0).unwrap().invert);
    assert!(canvas.resolved_text_style(1, 0).unwrap().invert);
    assert!(canvas.resolved_text_style(3, 0).unwrap().invert);
    assert_eq!(
        canvas.damage_region(),
        Some(DamageRegion {
            x: 1,
            y: 0,
            width: 3,
            height: 1,
        })
    );
}
