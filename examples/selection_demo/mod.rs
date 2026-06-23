use iocraft::{prelude::*, taffy::Size};
use std::time::{SystemTime, UNIX_EPOCH};

const GUTTER_WIDTH: usize = 4;
const BODY_HEIGHT: usize = 8;
const CLIPBOARD_ENV: &str = "IOCRAFT_EXAMPLE_WRITE_CLIPBOARD";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn preview(text: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut s = text.replace('\n', "\\n");
    if s.chars().count() > MAX_CHARS {
        s = s.chars().take(MAX_CHARS).collect::<String>();
        s.push('…');
    }
    s
}

fn write_text(canvas: &mut Canvas, x: isize, y: isize, text: &str, style: CanvasTextStyle) {
    canvas
        .subview_mut(0, 0, 0, 0, canvas.width(), canvas.height())
        .set_text(x, y, text, style);
}

fn write_link(
    canvas: &mut Canvas,
    x: isize,
    y: isize,
    text: &str,
    style: CanvasTextStyle,
    href: &str,
) {
    canvas
        .subview_mut(0, 0, 0, 0, canvas.width(), canvas.height())
        .set_text_with_link(x, y, text, style, Some(href));
}

fn mark_gutter_no_select(canvas: &mut Canvas, row: usize) {
    canvas.mark_no_select_region(0, row, GUTTER_WIDTH, 1);
}

fn style(color: Option<Color>, weight: Weight, underline: bool) -> CanvasTextStyle {
    let mut style = CanvasTextStyle::default();
    style.color = color;
    style.weight = weight;
    style.underline = underline;
    style
}

fn gutter_style() -> CanvasTextStyle {
    style(Some(Color::DarkGrey), Weight::Normal, false)
}

fn normal_style() -> CanvasTextStyle {
    CanvasTextStyle::default()
}

fn link_style() -> CanvasTextStyle {
    style(Some(Color::Cyan), Weight::Normal, true)
}

fn selection_canvas(width: usize) -> Canvas {
    let width = width.max(1);
    let mut canvas = Canvas::new(width, BODY_HEIGHT);

    write_text(
        &mut canvas,
        0,
        0,
        "iocraft fullscreen selection screen-buffer demo",
        style(Some(Color::Green), Weight::Bold, false),
    );

    for row in 1..=5 {
        mark_gutter_no_select(&mut canvas, row);
        write_text(
            &mut canvas,
            0,
            row as isize,
            &format!(" {row} │"),
            gutter_style(),
        );
    }

    write_link(
        &mut canvas,
        GUTTER_WIDTH as isize,
        1,
        "linked docs",
        link_style(),
        "https://docs.example.dev",
    );
    write_text(
        &mut canvas,
        (GUTTER_WIDTH + "linked docs".len()) as isize,
        1,
        " and https://plain.example/path).",
        normal_style(),
    );
    write_text(
        &mut canvas,
        GUTTER_WIDTH as isize,
        2,
        "The quick brown fox",
        normal_style(),
    );
    write_text(
        &mut canvas,
        GUTTER_WIDTH as isize,
        3,
        "jumps over the lazy dog",
        normal_style(),
    );
    write_text(
        &mut canvas,
        GUTTER_WIDTH as isize,
        4,
        "soft wrapped part one ",
        normal_style(),
    );
    write_text(
        &mut canvas,
        GUTTER_WIDTH as isize,
        5,
        "continued here",
        normal_style(),
    );
    canvas.mark_soft_wrap_continuation(5, GUTTER_WIDTH + "soft wrapped part one ".len());

    write_text(
        &mut canvas,
        0,
        6,
        "Drag selects · double-click word · triple-click row · Shift+arrows extend",
        style(Some(Color::DarkGrey), Weight::Normal, false),
    );
    write_text(
        &mut canvas,
        0,
        7,
        "Esc clears · Ctrl+C copies/would-copy · q quits · search highlight: lazy",
        style(Some(Color::DarkGrey), Weight::Normal, false),
    );

    canvas
}

fn highlighted_selection_canvas(width: usize, selection: &SelectionController) -> Canvas {
    let mut canvas = selection_canvas(width);
    canvas.apply_search_highlight("lazy", StyleOverlay::current_match(Color::DarkYellow));
    selection
        .selection()
        .apply_overlay(&mut canvas, StyleOverlay::selection_background(Color::Blue));
    canvas
}

#[derive(Default, Props)]
struct ScreenBufferPaneProps {
    width: u16,
    height: u16,
    selection: SelectionController,
    status: String,
    last_copied: String,
    write_clipboard: bool,
}

struct ScreenBufferPane {
    width: u16,
    height: u16,
    selection: SelectionController,
    status: String,
    last_copied: String,
    write_clipboard: bool,
}

impl Component for ScreenBufferPane {
    type Props<'a> = ScreenBufferPaneProps;

    fn new(props: &Self::Props<'_>) -> Self {
        Self {
            width: props.width,
            height: props.height,
            selection: props.selection.clone(),
            status: props.status.clone(),
            last_copied: props.last_copied.clone(),
            write_clipboard: props.write_clipboard,
        }
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.width = props.width;
        self.height = props.height;
        self.selection = props.selection.clone();
        self.status = props.status.clone();
        self.last_copied = props.last_copied.clone();
        self.write_clipboard = props.write_clipboard;
        let width = props.width;
        let height = props.height;
        updater.set_measure_func(Box::new(move |_, _, _| Size {
            width: width as f32,
            height: height as f32,
        }));
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        let width = drawer.size().width as usize;
        let height = drawer.size().height as usize;
        if width == 0 || height == 0 {
            return;
        }

        let body = highlighted_selection_canvas(width, &self.selection);
        let selected_text = self.selection.selected_text(&selection_canvas(width));
        let clipboard_mode = if self.write_clipboard {
            "OSC 52 clipboard writes enabled"
        } else {
            "simulated copy; set IOCRAFT_EXAMPLE_WRITE_CLIPBOARD=1 to write OSC 52"
        };
        let copied = if self.last_copied.is_empty() {
            "<none>".to_string()
        } else {
            preview(&self.last_copied)
        };

        let mut view = drawer.canvas();
        view.blit_region_from(&body, 0, 0, 0, 0, body.width(), body.height());

        let status_row = BODY_HEIGHT as isize + 1;
        if (status_row as usize) < height {
            view.set_background_color(0, status_row, width, 1, Color::DarkBlue);
            view.set_text(
                0,
                status_row,
                &format!("Status: {}", self.status),
                style(Some(Color::White), Weight::Bold, false),
            );
        }

        let selected_row = status_row + 1;
        if selected_row >= 0 && (selected_row as usize) < height {
            view.set_text(
                0,
                selected_row,
                &format!("Selected text: {:?}", selected_text),
                style(Some(Color::Green), Weight::Normal, false),
            );
        }

        let copied_row = selected_row + 1;
        if copied_row >= 0 && (copied_row as usize) < height {
            view.set_text(
                0,
                copied_row,
                &format!("Last copy: {copied}"),
                style(Some(Color::Yellow), Weight::Normal, false),
            );
        }

        let mode_row = copied_row + 1;
        if mode_row >= 0 && (mode_row as usize) < height {
            view.set_text(
                0,
                mode_row,
                clipboard_mode,
                style(Some(Color::DarkGrey), Weight::Normal, false),
            );
        }
    }
}

fn copy_status(prefix: &str, text: &str, write_clipboard: bool) -> String {
    if text.is_empty() {
        return format!("{prefix}: no selected text");
    }
    if write_clipboard {
        format!(
            "{prefix}: copied {} chars to clipboard: {}",
            text.chars().count(),
            preview(text)
        )
    } else {
        format!(
            "{prefix}: would copy {} chars: {}",
            text.chars().count(),
            preview(text)
        )
    }
}

#[component]
fn Example(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (terminal_width, terminal_height) = hooks.use_terminal_size();
    let mut system = hooks.use_context_mut::<SystemContext>();
    let (stdout, _) = hooks.use_output();
    let mut selection = hooks.use_state(SelectionController::new);
    let mut status = hooks.use_state(|| "Select text with the mouse.".to_string());
    let mut last_copied = hooks.use_state(String::new);
    let mut should_exit = hooks.use_state(|| false);
    let write_clipboard = std::env::var_os(CLIPBOARD_ENV).is_some();

    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => {
            if key.code == KeyCode::Char('q') && key.modifiers.is_empty() {
                should_exit.set(true);
                return;
            }
            let width = (terminal_width as usize).max(1);
            let body = selection_canvas(width);
            let message = {
                let mut controller = selection.write();
                match controller.handle_fullscreen_key_event(&key, body.width(), body.height()) {
                    FullscreenSelectionKeyOutcome::Ignored => return,
                    FullscreenSelectionKeyOutcome::CopyRequested => {
                        let text = controller.selected_text(&body);
                        if !text.is_empty() {
                            if write_clipboard {
                                stdout.set_clipboard(&text);
                            }
                            last_copied.set(text.clone());
                        }
                        copy_status("Ctrl+C", &text, write_clipboard)
                    }
                    FullscreenSelectionKeyOutcome::Cleared => "Selection cleared".to_string(),
                    FullscreenSelectionKeyOutcome::FocusMoved { movement, moved } => {
                        format!("Selection focus moved {movement:?}; moved={moved}")
                    }
                    other => format!("key outcome: {other:?}"),
                }
            };
            status.set(message);
        }
        TerminalEvent::FullscreenMouse(mouse) => {
            let width = (terminal_width as usize).max(1);
            let body = selection_canvas(width);
            if mouse.row as usize >= body.height() || mouse.column as usize >= body.width() {
                status.set("Mouse event outside the demo screen buffer".to_string());
                return;
            }
            let message = {
                let mut controller = selection.write();
                let outcome =
                    controller.handle_fullscreen_mouse_event(&body, &mouse, now_ms(), false);
                if let Some(text) = controller.copy_on_select_text(&body) {
                    if write_clipboard {
                        stdout.set_clipboard(&text);
                    }
                    last_copied.set(text.clone());
                    copy_status("copy-on-select", &text, write_clipboard)
                } else {
                    match outcome {
                        FullscreenSelectionEventOutcome::Release(release) => {
                            if let Some(href) = release.hyperlink {
                                format!("hyperlink fallback under click: {href}")
                            } else if release.was_dragging {
                                "Selection released; waiting for settled copy".to_string()
                            } else {
                                format!("mouse release: {release:?}")
                            }
                        }
                        FullscreenSelectionEventOutcome::Drag => {
                            format!("Dragging selection: {:?}", controller.selected_text(&body))
                        }
                        FullscreenSelectionEventOutcome::Wheel { cleared_selection } => {
                            format!("wheel event; cleared_selection={cleared_selection}")
                        }
                        other => format!("mouse outcome: {other:?}"),
                    }
                }
            };
            status.set(message);
        }
        _ => {}
    });

    if should_exit.get() {
        system.exit();
    }

    let selection_snapshot = selection.read().clone();
    let status_text = status.read().clone();
    let last_copied_text = last_copied.read().clone();

    element! {
        View(width: terminal_width, height: terminal_height, background_color: Color::Black) {
            ScreenBufferPane(
                width: terminal_width,
                height: terminal_height,
                selection: selection_snapshot,
                status: status_text,
                last_copied: last_copied_text,
                write_clipboard,
            )
        }
    }
}

pub fn main() {
    smol::block_on(element!(Example).fullscreen()).unwrap();
}
