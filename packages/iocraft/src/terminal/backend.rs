use super::*;

pub(super) trait TerminalImpl: Write + Send {
    fn refresh_size(&mut self) {}
    fn size(&self) -> Option<(u16, u16)> {
        None
    }
    fn set_size_from_resize_event(&mut self, _width: u16, _height: u16) {}
    fn set_mouse_capture(&mut self, _enabled: bool) -> io::Result<()> {
        Ok(())
    }

    /// Re-asserts terminal modes that some emulators drop during resize.
    fn reassert_after_resize(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Re-asserts terminal modes after a long stdin silence gap.
    ///
    /// This mirrors CC Ink's `STDIN_RESUME_GAP_MS` self-heal for tmux attach,
    /// SSH reconnect, and laptop sleep/wake. It should be non-destructive: do
    /// not clear/re-enter the alternate screen here, only restore idempotent or
    /// stack-balanced modes such as extended keys and mouse tracking.
    fn reassert_after_stdin_resume(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Replaces the keyboard enhancement (kitty protocol) flags requested from the
    /// terminal. If enhancement is already active, the old flags are popped and the
    /// new ones pushed immediately; otherwise the new flags take effect the next time
    /// raw mode is enabled.
    fn set_keyboard_enhancement_flags(
        &mut self,
        _flags: event::KeyboardEnhancementFlags,
    ) -> io::Result<()> {
        Ok(())
    }

    /// Polls for a pending "resumed from suspension" signal (SIGCONT on unix). Used by
    /// [`Terminal::wait`] to wake the render loop so it can repair the display after the
    /// user foregrounds the process (e.g. Ctrl+Z followed by `fg`).
    fn poll_resumed(&mut self, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }

    /// Returns `true` (and clears the flag) if the process was resumed from suspension
    /// since the last call.
    fn take_resumed(&mut self) -> bool {
        false
    }

    /// Re-applies terminal modes and clears cached output state after the process was
    /// resumed from suspension. The shell typically restores cooked mode while the app
    /// is stopped, so raw mode (and friends) must be re-enabled unconditionally, and the
    /// next canvas write must not assume anything previously on screen survived.
    fn reinitialize_after_resume(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Hands the terminal back to the shell and suspends the process (Ctrl+Z on
    /// Unix). Non-Unix backends or non-interactive inputs may treat this as a no-op.
    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Positions the physical terminal cursor. When `visible` is true (ratatui model),
    /// the cursor is shown; when false (ink model), it is positioned for IME only.
    /// Called after each canvas write.
    fn position_cursor(
        &mut self,
        _declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> io::Result<()> {
        Ok(())
    }

    fn position_cursor_would_write(
        &self,
        _declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> bool {
        false
    }

    fn is_raw_mode_supported(&self) -> bool {
        false
    }
    fn is_raw_mode_enabled(&self) -> bool;
    fn set_raw_mode_enabled(&mut self, _raw_mode_enabled: bool) -> io::Result<()> {
        Ok(())
    }
    fn is_fullscreen(&self) -> bool {
        false
    }
    fn set_dynamic_alternate_screen(
        &mut self,
        _request: Option<crate::context::AlternateScreenRequest>,
    ) -> io::Result<bool> {
        Ok(false)
    }
    fn set_decstbm_safe(&mut self, _safe: bool) {}
    fn clear_canvas_would_write(&self) -> bool {
        true
    }
    fn clear_canvas(&mut self) -> io::Result<()>;
    fn clear_screen(&mut self) -> io::Result<()> {
        self.clear_canvas()
    }
    fn clear_terminal(&mut self) -> io::Result<()> {
        self.clear_canvas()
    }
    #[cfg_attr(not(test), allow(dead_code))]
    fn write_canvas_would_write(&self, _prev: Option<&Canvas>, _canvas: &Canvas) -> bool {
        true
    }
    fn write_canvas(&mut self, prev: Option<&Canvas>, canvas: &Canvas) -> io::Result<()>;
    fn event_stream(&mut self) -> io::Result<BoxStream<'static, TerminalEvent>>;
    fn dest(&mut self) -> &mut dyn Write;
    fn alt(&mut self) -> &mut dyn Write;
}

/// State shared between the SIGCONT listener thread and the render loop.
#[cfg(unix)]
struct ResumeSignalShared {
    resumed: std::sync::atomic::AtomicBool,
    waker: Mutex<Option<Waker>>,
}

/// Listens for SIGCONT on a dedicated thread and records it in shared state.
///
/// signal-hook's iterator API is used so the actual signal handler only performs an
/// async-signal-safe self-pipe write; the (signal-unsafe) waker invocation happens on
/// this listener thread, in normal thread context.
#[cfg(unix)]
pub(super) struct ResumeSignalListener {
    shared: Arc<ResumeSignalShared>,
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl ResumeSignalListener {
    fn new() -> io::Result<Self> {
        use std::sync::atomic::AtomicBool;

        let shared = Arc::new(ResumeSignalShared {
            resumed: AtomicBool::new(false),
            waker: Mutex::new(None),
        });
        let mut signals =
            signal_hook::iterator::Signals::new([signal_hook::consts::signal::SIGCONT])?;
        let handle = signals.handle();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("iocraft-sigcont".into())
            .spawn(move || {
                for _ in &mut signals {
                    thread_shared
                        .resumed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(waker) = thread_shared.waker.lock().unwrap().take() {
                        waker.wake();
                    }
                }
            })?;
        Ok(Self {
            shared,
            handle,
            thread: Some(thread),
        })
    }

    fn poll_resumed(&self, cx: &mut Context<'_>) -> Poll<()> {
        use std::sync::atomic::Ordering;
        if self.shared.resumed.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        *self.shared.waker.lock().unwrap() = Some(cx.waker().clone());
        // Re-check to close the race where the signal arrived between the first load
        // and the waker registration.
        if self.shared.resumed.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn take_resumed(&self) -> bool {
        self.shared
            .resumed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    fn mark_resumed(&self) {
        self.shared
            .resumed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(waker) = self.shared.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

#[cfg(unix)]
impl Drop for ResumeSignalListener {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) fn clear_canvas_inline(
    dest: &mut (impl Write + ?Sized),
    prev_canvas_height: u16,
) -> io::Result<()> {
    let lines_to_rewind = prev_canvas_height - 1;
    if lines_to_rewind == 0 {
        dest.queue(cursor::MoveToColumn(0))?
            .queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        Ok(())
    } else {
        dest.queue(cursor::MoveToPreviousLine(lines_to_rewind as _))?
            .queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        Ok(())
    }
}

/// Global bookkeeping for the panic hook that restores the terminal to a usable state
/// before the default hook prints the panic message and backtrace.
///
/// Without this, a panic in fullscreen mode is invisible: the message goes to the
/// alternate screen, which is discarded when the process exits and the shell restores
/// the main screen. Raw mode similarly survives the panic, leaving the user's shell
/// with no echo and broken line input.
pub(super) struct PanicRestoreState {
    /// Number of live `StdTerminal`s that have modified terminal state. The hook is a
    /// no-op when this is zero (e.g. after a clean shutdown).
    pub(super) live_terminals: usize,
    /// Number of live terminals in fullscreen (alternate screen) mode.
    pub(super) fullscreen_terminals: usize,
}

pub(super) static PANIC_RESTORE_STATE: Mutex<PanicRestoreState> = Mutex::new(PanicRestoreState {
    live_terminals: 0,
    fullscreen_terminals: 0,
});

static INSTALL_PANIC_HOOK: std::sync::Once = std::sync::Once::new();

/// Best-effort terminal restoration, safe to call from a panic hook. Writes go directly
/// to stdout: raw mode is a tty-level state (restored via ioctl, not the stream), and
/// the escape sequences reach the same tty regardless of which stream the renderer was
/// configured to use.
fn restore_terminal_for_panic() {
    let state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.live_terminals == 0 {
        return;
    }
    let mut stdout = io::stdout();
    let _ = terminal::disable_raw_mode();
    let _ = stdout.queue(event::DisableBracketedPaste);
    let _ = stdout.queue(event::DisableFocusChange);
    let _ = stdout.queue(event::DisableMouseCapture);
    if state.fullscreen_terminals > 0 {
        let _ = stdout.queue(terminal::LeaveAlternateScreen);
    }
    let _ = stdout.queue(cursor::Show);
    let _ = stdout.flush();
}

pub(super) fn register_terminal_for_panic_restore(fullscreen: bool) {
    INSTALL_PANIC_HOOK.call_once(|| {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_for_panic();
            prev_hook(info);
        }));
    });
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.live_terminals += 1;
    if fullscreen {
        state.fullscreen_terminals += 1;
    }
}

fn register_fullscreen_mode_for_panic_restore() {
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.fullscreen_terminals += 1;
}

fn unregister_fullscreen_mode_for_panic_restore() {
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.fullscreen_terminals = state.fullscreen_terminals.saturating_sub(1);
}

pub(super) fn unregister_terminal_for_panic_restore(fullscreen: bool) {
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.live_terminals = state.live_terminals.saturating_sub(1);
    if fullscreen {
        state.fullscreen_terminals = state.fullscreen_terminals.saturating_sub(1);
    }
    if state.live_terminals == 0 {
        state.fullscreen_terminals = 0;
    }
}

pub(super) struct StdTerminal<'a> {
    pub(super) input_is_terminal: bool,
    pub(super) dest: Box<dyn Write + Send + 'a>,
    pub(super) alt: Box<dyn Write + Send + 'a>,
    pub(super) fullscreen: bool,
    pub(super) mouse_capture: bool,
    pub(super) dynamic_alternate_saved_mouse_capture: Option<bool>,
    pub(super) raw_mode_enabled: bool,
    pub(super) enabled_keyboard_enhancement: bool,
    pub(super) keyboard_enhancement_flags: event::KeyboardEnhancementFlags,
    pub(super) prev_canvas_top_row: u16,
    pub(super) prev_canvas_height: u16,
    pub(super) prev_size_on_write: Option<(u16, u16)>,
    pub(super) size: Option<(u16, u16)>,
    /// Whether the physical cursor is currently shown (via a cursor declaration).
    pub(super) cursor_visible: bool,
    /// In inline mode, how many rows above the canvas-bottom baseline the cursor was
    /// moved by the last cursor declaration. The row-diff logic in `write_canvas` and
    /// `clear_canvas` assumes the cursor sits on the canvas's last row, so this
    /// displacement must be undone (see `restore_cursor_baseline`) before either runs.
    pub(super) cursor_displacement_rows: u16,
    /// Whether the last inline canvas write ended in the terminal's right-margin
    /// auto-wrap pending state. VT terminals typically delay wrapping until the next
    /// printable byte; before relative cursor movement we resolve that pending state
    /// with CR, mirroring Ink/log-update's cursor model.
    pub(super) inline_pending_wrap: bool,
    /// Whether it is safe to emit a DECSTBM scroll patch before the row diff. CC
    /// only enables this optimization when the whole scroll+diff sequence is
    /// atomic (DEC 2026 synchronized update); otherwise users can see the region
    /// jump before edge rows are repainted.
    pub(super) decstbm_safe: bool,
    /// The first inline diff after a fresh initial frame is treated as
    /// contaminated and repainted in full. This mirrors Ink's contaminated-frame
    /// guard: after process startup/re-entry we do not fully control what rows
    /// precede the canvas in the main screen, so establish a clean retained
    /// baseline before trusting sparse row diffs.
    pub(super) inline_force_full_rewrite_next_diff: bool,
    #[cfg(unix)]
    pub(super) resume_signal: Option<ResumeSignalListener>,
}

impl Write for StdTerminal<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.dest.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.dest.flush()
    }
}

impl TerminalImpl for StdTerminal<'_> {
    fn refresh_size(&mut self) {
        self.size = terminal::size().ok()
    }

    fn size(&self) -> Option<(u16, u16)> {
        self.size
    }

    fn set_size_from_resize_event(&mut self, width: u16, height: u16) {
        self.size = Some((width, height));
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> io::Result<()> {
        if self.mouse_capture != enabled {
            self.mouse_capture = enabled;
            if self.raw_mode_enabled {
                if enabled {
                    self.dest.execute(event::EnableMouseCapture)?;
                } else {
                    self.dest.execute(event::DisableMouseCapture)?;
                }
            }
        }
        Ok(())
    }

    fn reassert_after_resize(&mut self) -> io::Result<()> {
        // Mirrors CC Ink's alt-screen resize self-heal: some terminals reset
        // mouse tracking on resize. Re-emit the mode without consulting the
        // cached boolean, because the whole point is to repair terminal-side
        // state that may have diverged from our retained state.
        if self.raw_mode_enabled {
            self.dest.execute(event::EnableFocusChange)?;
            if self.mouse_capture {
                self.dest.execute(event::EnableMouseCapture)?;
            }
        }
        Ok(())
    }

    fn reassert_after_stdin_resume(&mut self) -> io::Result<()> {
        // Match CC Ink's stdin-gap recovery: restore non-destructive terminal
        // modes that reconnect/sleep can drop. Kitty keyboard is stack-based,
        // so pop-before-push via reassert_keyboard_enhancement() prevents
        // accumulating protocol depth on every idle gap.
        if self.raw_mode_enabled {
            self.reassert_keyboard_enhancement()?;
            if self.mouse_capture {
                self.dest.execute(event::EnableMouseCapture)?;
            }
        }
        Ok(())
    }

    fn set_keyboard_enhancement_flags(
        &mut self,
        flags: event::KeyboardEnhancementFlags,
    ) -> io::Result<()> {
        if self.keyboard_enhancement_flags != flags {
            self.keyboard_enhancement_flags = flags;
            if self.enabled_keyboard_enhancement {
                // Swap the active flags in place.
                self.dest.execute(event::PopKeyboardEnhancementFlags)?;
                self.dest
                    .execute(event::PushKeyboardEnhancementFlags(flags))?;
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn poll_resumed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match &self.resume_signal {
            Some(signal) => signal.poll_resumed(cx),
            None => Poll::Pending,
        }
    }

    #[cfg(unix)]
    fn take_resumed(&mut self) -> bool {
        self.resume_signal
            .as_ref()
            .is_some_and(|signal| signal.take_resumed())
    }

    fn reinitialize_after_resume(&mut self) -> io::Result<()> {
        // The shell restored cooked mode while we were stopped, but our bookkeeping
        // still says raw mode is on. Re-apply every terminal mode unconditionally,
        // bypassing apply_raw_mode_enabled's change detection.
        if self.raw_mode_enabled {
            terminal::enable_raw_mode()?;
            self.reassert_keyboard_enhancement()?;
            if self.mouse_capture && !self.fullscreen {
                self.dest.execute(event::EnableMouseCapture)?;
            }
            self.dest.execute(event::EnableFocusChange)?;
            self.dest.execute(event::EnableBracketedPaste)?;
        }
        self.dest.queue(cursor::Hide)?;
        self.cursor_visible = false;
        self.cursor_displacement_rows = 0;
        if self.fullscreen {
            // Re-entering the alternate screen after SIGCONT/shell handoff must
            // be destructive, matching CC Ink's reenterAltScreen(): the terminal
            // may have dropped mode 1049 or preserved stale alt-buffer cells.
            // Enter alt-screen, erase it, and home the cursor so the next full
            // write starts from a known blank anchor.
            self.dest
                .queue(terminal::EnterAlternateScreen)?
                .queue(terminal::Clear(terminal::ClearType::All))?
                .queue(cursor::MoveTo(0, 0))?;
            if self.mouse_capture {
                self.dest.execute(event::EnableMouseCapture)?;
            }
        }
        // Anything we previously drew can no longer be trusted: in inline mode the
        // shell printed over our output; in fullscreen mode we just re-entered the
        // alternate screen, which starts blank. Treat the next write as a first
        // write so the canvas is fully re-rendered rather than row-diffed.
        self.prev_canvas_height = 0;
        self.prev_canvas_top_row = 0;
        self.prev_size_on_write = None;
        self.inline_pending_wrap = false;
        self.inline_force_full_rewrite_next_diff = false;
        Ok(())
    }

    fn suspend(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            if !self.input_is_terminal {
                return Ok(());
            }

            // Match CC Ink's Ctrl+Z handoff: leave the terminal in cooked mode,
            // show the cursor, and disable input side modes so the shell prompt
            // does not receive bracketed-paste/focus/mouse/extended-key noise
            // while this process is stopped. Keep our bookkeeping intact so the
            // SIGCONT repair path can reassert the modes that were active.
            self.restore_cursor_baseline()?;
            if self.raw_mode_enabled {
                terminal::disable_raw_mode()?;
                self.dest.execute(event::DisableBracketedPaste)?;
                self.dest.execute(event::DisableFocusChange)?;
                if self.mouse_capture {
                    self.dest.execute(event::DisableMouseCapture)?;
                }
                if self.enabled_keyboard_enhancement {
                    self.dest.execute(event::PopKeyboardEnhancementFlags)?;
                }
            }
            self.dest.queue(cursor::Show)?;
            self.dest.flush()?;

            signal_hook::low_level::raise(signal_hook::consts::signal::SIGSTOP)
                .map_err(io::Error::other)?;
            if let Some(signal) = &self.resume_signal {
                signal.mark_resumed();
            }
        }
        Ok(())
    }

    fn is_raw_mode_supported(&self) -> bool {
        self.input_is_terminal
    }

    fn is_raw_mode_enabled(&self) -> bool {
        self.raw_mode_enabled
    }

    fn set_raw_mode_enabled(&mut self, raw_mode_enabled: bool) -> io::Result<()> {
        self.apply_raw_mode_enabled(raw_mode_enabled)
    }

    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    fn set_dynamic_alternate_screen(
        &mut self,
        request: Option<crate::context::AlternateScreenRequest>,
    ) -> io::Result<bool> {
        match (self.fullscreen, request) {
            (false, Some(request)) => {
                self.restore_cursor_baseline()?;
                if self.cursor_visible {
                    self.dest.queue(cursor::Hide)?;
                    self.cursor_visible = false;
                }
                self.dest
                    .queue(terminal::EnterAlternateScreen)?
                    .queue(terminal::Clear(terminal::ClearType::All))?
                    .queue(cursor::MoveTo(0, 0))?;
                self.fullscreen = true;
                self.dynamic_alternate_saved_mouse_capture = Some(self.mouse_capture);
                register_fullscreen_mode_for_panic_restore();
                self.set_mouse_capture(request.mouse_tracking)?;
                self.reset_retained_output_state();
                self.dest.flush()?;
                Ok(true)
            }
            (true, Some(request)) => {
                self.set_mouse_capture(request.mouse_tracking)?;
                Ok(false)
            }
            (true, None) => {
                self.restore_cursor_baseline()?;
                let restore_mouse_capture = self
                    .dynamic_alternate_saved_mouse_capture
                    .take()
                    .unwrap_or(false);
                self.set_mouse_capture(restore_mouse_capture)?;
                self.dest.queue(terminal::LeaveAlternateScreen)?;
                self.fullscreen = false;
                unregister_fullscreen_mode_for_panic_restore();
                self.reset_retained_output_state();
                self.dest.flush()?;
                Ok(true)
            }
            (false, None) => Ok(false),
        }
    }

    fn set_decstbm_safe(&mut self, safe: bool) {
        self.decstbm_safe = safe;
    }

    fn position_cursor(
        &mut self,
        declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        match declaration {
            Some(decl) => {
                let (mut x, mut y) = (decl.x as u16, decl.y as u16);
                if self.fullscreen {
                    if let Some((cols, rows)) = self.size {
                        x = x.min(cols.saturating_sub(1));
                        y = y.min(rows.saturating_sub(1));
                    }
                    self.dest
                        .queue(cursor::MoveTo(x, self.prev_canvas_top_row + y))?;
                } else {
                    self.restore_cursor_baseline()?;
                    let last_row = self.prev_canvas_height.saturating_sub(1);
                    let rows_up = last_row.saturating_sub(y);
                    self.move_inline_to_previous_line(rows_up)?;
                    self.move_inline_to_column(x)?;
                    self.cursor_displacement_rows = rows_up;
                }
                if decl.visible && !self.cursor_visible {
                    self.dest.queue(cursor::SetCursorStyle::SteadyBlock)?;
                    self.dest.queue(cursor::Show)?;
                    self.cursor_visible = true;
                } else if !decl.visible && self.cursor_visible {
                    self.dest.queue(cursor::Hide)?;
                    self.cursor_visible = false;
                }
            }
            None => {
                if self.cursor_visible {
                    self.dest.queue(cursor::Hide)?;
                    self.cursor_visible = false;
                }
            }
        }
        self.dest.flush()?;
        Ok(())
    }

    fn position_cursor_would_write(
        &self,
        declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> bool {
        self.inline_pending_wrap
            || match declaration {
                Some(_) => true,
                None => self.cursor_visible,
            }
    }

    fn clear_canvas_would_write(&self) -> bool {
        self.inline_pending_wrap || self.cursor_displacement_rows > 0 || self.prev_canvas_height > 0
    }

    fn clear_canvas(&mut self) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        if self.prev_canvas_height == 0 {
            return Ok(());
        }

        if self.fullscreen {
            self.dest
                .queue(cursor::MoveTo(0, self.prev_canvas_top_row))?
                .queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
            return Ok(());
        }

        if let Some(size) = self.size {
            if self.prev_canvas_height >= size.1 {
                // A viewport-tall inline canvas cannot be cleared reliably with
                // relative line erases: some rows may already be in native
                // scrollback. Clear the visible screen and repaint from a fresh
                // baseline, but do NOT purge scrollback. Local JSX overlays such
                // as /resume must preserve the user's mouse-wheel terminal history.
                self.dest
                    .queue(terminal::Clear(terminal::ClearType::All))?
                    .queue(cursor::MoveTo(0, 0))?;
                return Ok(());
            }
        }

        clear_canvas_inline(&mut *self.dest, self.prev_canvas_height)
    }

    fn clear_screen(&mut self) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        self.dest
            .queue(terminal::Clear(terminal::ClearType::All))?
            .queue(cursor::MoveTo(0, 0))?;
        self.reset_retained_output_state();
        Ok(())
    }

    fn clear_terminal(&mut self) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        self.dest.write_all(clear_terminal_sequence().as_bytes())?;
        self.reset_retained_output_state();
        Ok(())
    }

    fn write_canvas_would_write(&self, prev: Option<&Canvas>, canvas: &Canvas) -> bool {
        if self.inline_pending_wrap || self.cursor_displacement_rows > 0 {
            return true;
        }

        let Some(prev) = prev else {
            return self.fullscreen || canvas.height() > 0;
        };

        if self.fullscreen {
            if self.prev_size_on_write != self.size {
                return true;
            }
            if canvas.should_force_full_repaint() {
                return prev.height().max(canvas.height()) > 0;
            }
            if self
                .fullscreen_scroll_hint_candidate(prev, canvas)
                .is_some()
            {
                return true;
            }
            let max_height = prev.height().max(canvas.height());
            return (0..max_height).any(|y| prev.row_change_start(canvas, y).is_some());
        }

        if self.inline_force_full_rewrite_next_diff || self.inline_resize_requires_full_rewrite() {
            return self.prev_canvas_height > 0 || canvas.height() > 0;
        }
        if canvas.should_force_full_repaint() {
            return canvas.height() > self.inline_unreachable_rows_for_diff(prev.height())
                || self.prev_canvas_height > 0;
        }

        let prev_height = prev.height();
        let new_height = canvas.height();
        if self.inline_shrink_requires_full_rewrite(prev_height, new_height) {
            return self.prev_canvas_height > 0 || canvas.height() > 0;
        }

        let max_height = prev_height.max(new_height);
        (0..max_height).any(|y| {
            if prev.row_change_start(canvas, y).is_none() {
                return false;
            }
            !(y < self.inline_unreachable_rows_for_diff(prev_height) && prev.row_eq(canvas, y))
        })
    }

    fn write_canvas(&mut self, prev: Option<&Canvas>, canvas: &Canvas) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        let Some(prev) = prev else {
            // No previous canvas: full write.
            if self.fullscreen {
                self.prev_canvas_top_row = 0;
                self.dest.queue(cursor::MoveTo(0, 0))?;
            }
            self.prev_canvas_height = canvas.height() as _;
            self.prev_size_on_write = self.size;
            if self.fullscreen {
                self.write_fullscreen_canvas_absolute(canvas)?;
                self.park_fullscreen_cursor_after_write(canvas.height())?;
            } else {
                self.write_inline_canvas_without_final_newline(canvas)?;
                self.inline_force_full_rewrite_next_diff = true;
            }
            return Ok(());
        };

        if self.fullscreen {
            if self.prev_size_on_write != self.size {
                // If the terminal is changing size, clear it to make sure we don't leave
                // artifacts. This is especially important when the terminal is shrinking, since
                // characters might flow outside of the visible terminal, where they can't be
                // cleared with `\033[K` and oddly may re-enter the terminal as visible characters
                // are cleared.
                self.clear_fullscreen_screen()?;
                self.prev_canvas_height = canvas.height() as _;
                self.prev_canvas_top_row = 0;
                self.prev_size_on_write = self.size;
                self.write_fullscreen_canvas_absolute(canvas)?;
                self.park_fullscreen_cursor_after_write(canvas.height())?;
                return Ok(());
            }

            let force_full_repaint = canvas.should_force_full_repaint();
            let scroll_hint = if force_full_repaint {
                None
            } else {
                self.fullscreen_scroll_hint(prev, canvas)
            };
            let shifted_prev_storage = if let Some(hint) = scroll_hint {
                self.write_fullscreen_scroll_patch(hint)?;
                let mut shifted = prev.clone();
                shifted.shift_rows(hint.top, hint.bottom, hint.delta);
                Some(shifted)
            } else {
                None
            };
            let mut wrote_anything = scroll_hint.is_some();
            let prev_for_diff = shifted_prev_storage.as_ref().unwrap_or(prev);

            // Fullscreen: absolute positioning.
            let top_row = self.prev_canvas_top_row;
            let max_height = prev_for_diff.height().max(canvas.height());
            for y in 0..max_height {
                let start_col = if force_full_repaint || y >= canvas.height() {
                    Some(0)
                } else {
                    prev_for_diff.row_change_start(canvas, y)
                };
                let Some(start_col) = start_col else {
                    continue;
                };

                wrote_anything = true;
                self.dest
                    .queue(cursor::MoveTo(start_col as u16, top_row + y as u16))?;
                if y < canvas.height() {
                    canvas.write_ansi_row_from_col_without_newline(
                        y,
                        start_col,
                        &mut *self.dest,
                    )?;
                } else {
                    self.dest
                        .queue(terminal::Clear(terminal::ClearType::CurrentLine))?;
                }
            }
            if wrote_anything {
                self.park_fullscreen_cursor_after_write(canvas.height())?;
            }
            self.prev_canvas_height = canvas.height() as _;
            return Ok(());
        }

        if self.inline_force_full_rewrite_next_diff || self.inline_resize_requires_full_rewrite() {
            self.clear_canvas()?;
            self.prev_canvas_height = canvas.height() as _;
            self.prev_size_on_write = self.size;
            self.write_inline_canvas_without_final_newline(canvas)?;
            self.inline_force_full_rewrite_next_diff = false;
            return Ok(());
        }

        if canvas.should_force_full_repaint() {
            if !self.inline_shrink_requires_full_rewrite(prev.height(), canvas.height())
                && self.write_inline_full_repaint(prev, canvas)?
            {
                self.prev_canvas_height = canvas.height() as _;
                self.prev_size_on_write = self.size;
                return Ok(());
            }

            self.clear_canvas()?;
            self.prev_canvas_height = canvas.height() as _;
            self.prev_size_on_write = self.size;
            self.write_inline_canvas_without_final_newline(canvas)?;
            self.inline_force_full_rewrite_next_diff = false;
            return Ok(());
        }
        self.prev_size_on_write = self.size;

        // Inline: row diff with relative cursor movement.
        let prev_height = prev.height();
        let new_height = canvas.height();

        if self.inline_shrink_requires_full_rewrite(prev_height, new_height) {
            self.clear_canvas()?;
            self.prev_canvas_height = canvas.height() as _;
            self.write_inline_canvas_without_final_newline(canvas)?;
            self.inline_force_full_rewrite_next_diff = false;
            return Ok(());
        }

        let max_height = prev_height.max(new_height);
        let mut current_y = prev_height.saturating_sub(1);

        for y in 0..max_height {
            let Some(mut start_col) = prev.row_change_start(canvas, y) else {
                continue;
            };
            if y >= new_height {
                start_col = 0;
            }
            // If a changed row has scrolled off the top of the reachable area,
            // we can't reach it with cursor movement — fall back to full rewrite.
            // The reachable boundary includes Ink/log-update's cursorRestoreScroll
            // guard: when the previous frame filled/overflowed the viewport, the
            // top visible row is also treated as unsafe because cursor restoration
            // can push one additional row into scrollback on real terminals.
            if y < self.inline_unreachable_rows_for_diff(prev_height) {
                if prev.row_eq(canvas, y) {
                    continue;
                }
                self.clear_canvas()?;
                self.prev_canvas_height = canvas.height() as _;
                self.write_inline_canvas_without_final_newline(canvas)?;
                self.inline_force_full_rewrite_next_diff = false;
                return Ok(());
            }
            let row_move_left_at_col0 = match y.cmp(&current_y) {
                std::cmp::Ordering::Less => {
                    self.move_inline_to_previous_line((current_y - y) as u16)?;
                    true
                }
                std::cmp::Ordering::Greater => {
                    // Lines within the previous canvas already exist in the
                    // terminal and can be reached with MoveToNextLine (CSI E).
                    // Lines beyond prev_height don't exist yet — we must emit
                    // \r\n to create them, since CSI E won't extend the
                    // scrollback when the cursor is at the bottom of the screen.
                    let last_existing_line = prev_height.saturating_sub(1).max(current_y);
                    if y <= last_existing_line {
                        self.move_inline_to_next_line((y - current_y) as u16)?;
                    } else {
                        let move_to_last = last_existing_line.saturating_sub(current_y);
                        if move_to_last > 0 {
                            self.move_inline_to_next_line(move_to_last as u16)?;
                        }
                        let new_lines = y - last_existing_line;
                        for _ in 0..new_lines {
                            self.write_inline_newline()?;
                        }
                    }
                    true
                }
                std::cmp::Ordering::Equal => false,
            };
            if start_col > 0 || !row_move_left_at_col0 {
                self.move_inline_to_column(start_col as u16)?;
            }
            current_y = y;

            if y < new_height {
                self.write_inline_canvas_row_from_col_without_newline(canvas, y, start_col)?;
            } else {
                self.dest
                    .queue(terminal::Clear(terminal::ClearType::CurrentLine))?;
            }
        }

        // Reposition cursor to last row of new canvas.
        let target_y = new_height.saturating_sub(1);
        match target_y.cmp(&current_y) {
            std::cmp::Ordering::Greater => {
                self.move_inline_to_next_line((target_y - current_y) as u16)?;
            }
            std::cmp::Ordering::Less => {
                self.move_inline_to_previous_line((current_y - target_y) as u16)?;
            }
            std::cmp::Ordering::Equal => {}
        }

        self.prev_canvas_height = new_height as _;
        Ok(())
    }

    fn event_stream(&mut self) -> io::Result<BoxStream<'static, TerminalEvent>> {
        if !self.input_is_terminal {
            return Ok(stream::pending().boxed());
        }

        self.apply_raw_mode_enabled(true)?;

        Ok(EventStream::new()
            .filter_map(|event| async move {
                match event {
                    Ok(Event::Key(event)) => Some(TerminalEvent::Key(KeyEvent {
                        code: event.code,
                        modifiers: event.modifiers,
                        kind: event.kind,
                    })),
                    Ok(Event::Mouse(event)) => {
                        Some(TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                            modifiers: event.modifiers,
                            column: event.column,
                            row: event.row,
                            cell_is_blank: false,
                            kind: event.kind,
                        }))
                    }
                    Ok(Event::Resize(width, height)) => Some(TerminalEvent::Resize(width, height)),
                    Ok(Event::FocusGained) => Some(TerminalEvent::FocusGained),
                    Ok(Event::FocusLost) => Some(TerminalEvent::FocusLost),
                    Ok(Event::Paste(text)) => Some(TerminalEvent::Paste(text)),
                    _ => None,
                }
            })
            .boxed())
    }

    fn dest(&mut self) -> &mut dyn Write {
        &mut *self.dest
    }

    fn alt(&mut self) -> &mut dyn Write {
        &mut *self.alt
    }
}

impl<'a> StdTerminal<'a> {
    pub(super) fn new(
        dest: Box<dyn Write + Send + 'a>,
        alt: Box<dyn Write + Send + 'a>,
        fullscreen: bool,
        mouse_capture: bool,
    ) -> io::Result<Self> {
        let mut term = Self {
            dest,
            alt,
            input_is_terminal: stdin().is_terminal(),
            fullscreen,
            mouse_capture,
            dynamic_alternate_saved_mouse_capture: None,
            raw_mode_enabled: false,
            enabled_keyboard_enhancement: false,
            keyboard_enhancement_flags: event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
            prev_canvas_top_row: 0,
            prev_canvas_height: 0,
            size: None,
            prev_size_on_write: None,
            cursor_visible: false,
            cursor_displacement_rows: 0,
            inline_pending_wrap: false,
            decstbm_safe: false,
            inline_force_full_rewrite_next_diff: false,
            // Best-effort: if signal registration fails, the terminal still works —
            // it just won't self-heal after suspend/resume.
            #[cfg(unix)]
            resume_signal: ResumeSignalListener::new().ok(),
        };
        term.dest.queue(cursor::Hide)?;
        if fullscreen {
            // Match Ink's <AlternateScreen> mount behavior: enter the
            // alternate buffer, erase any stale cells that a terminal may have
            // preserved, and anchor the first frame at the origin before any
            // canvas diff can run.
            term.dest
                .queue(terminal::EnterAlternateScreen)?
                .queue(terminal::Clear(terminal::ClearType::All))?
                .queue(cursor::MoveTo(0, 0))?;
        }
        register_terminal_for_panic_restore(fullscreen);
        Ok(term)
    }

    fn clear_fullscreen_screen(&mut self) -> io::Result<()> {
        self.dest
            .queue(terminal::Clear(terminal::ClearType::All))?
            .queue(cursor::MoveTo(0, 0))?;
        Ok(())
    }

    fn write_fullscreen_canvas_absolute(&mut self, canvas: &Canvas) -> io::Result<()> {
        if canvas.height() == 0 {
            return Ok(());
        }

        // CC Ink seeds alt-screen with a full-size blank previous frame so the
        // first paint uses cursor addressing instead of LF-based full-frame
        // rendering. Do the same at row granularity: absolute CUP per row never
        // scrolls the alt buffer, even when the canvas fills the viewport.
        self.dest.write_all(b"\x1b[0m")?;
        for y in 0..canvas.height() {
            self.dest
                .queue(cursor::MoveTo(0, self.prev_canvas_top_row + y as u16))?;
            canvas.write_ansi_row_without_newline(y, &mut *self.dest)?;
        }
        Ok(())
    }

    fn reset_retained_output_state(&mut self) {
        self.prev_canvas_height = 0;
        self.prev_canvas_top_row = 0;
        self.prev_size_on_write = None;
        self.cursor_displacement_rows = 0;
        self.inline_pending_wrap = false;
        self.inline_force_full_rewrite_next_diff = false;
    }

    fn park_fullscreen_cursor_after_write(&mut self, canvas_height: usize) -> io::Result<()> {
        if !self.fullscreen {
            return Ok(());
        }
        let target_row = self
            .size
            .map(|(_cols, rows)| rows.saturating_sub(1))
            .unwrap_or_else(|| {
                self.prev_canvas_top_row
                    .saturating_add(canvas_height.saturating_sub(1) as u16)
            });
        self.dest.queue(cursor::MoveTo(0, target_row))?;
        Ok(())
    }

    fn fullscreen_scroll_hint(
        &self,
        prev: &Canvas,
        canvas: &Canvas,
    ) -> Option<crate::canvas::ScrollHint> {
        if !self.decstbm_safe {
            return None;
        }
        self.fullscreen_scroll_hint_candidate(prev, canvas)
    }

    fn fullscreen_scroll_hint_candidate(
        &self,
        prev: &Canvas,
        canvas: &Canvas,
    ) -> Option<crate::canvas::ScrollHint> {
        if !self.fullscreen || self.prev_canvas_top_row != 0 {
            return None;
        }
        let hint = canvas.scroll_hint()?;
        if hint.delta == 0 || hint.top > hint.bottom {
            return None;
        }
        if hint.bottom >= prev.height() || hint.bottom >= canvas.height() {
            return None;
        }
        let region_height = hint.bottom - hint.top + 1;
        let abs_delta = hint.delta.unsigned_abs() as usize;
        if abs_delta == 0 || abs_delta >= region_height {
            return None;
        }
        Some(hint)
    }

    fn write_fullscreen_scroll_patch(&mut self, hint: crate::canvas::ScrollHint) -> io::Result<()> {
        // DECSTBM uses 1-indexed inclusive margins. SU (S) scrolls the region
        // up; SD (T) scrolls it down. Resetting the scroll region and homing the
        // cursor mirrors CC Ink's defensive sequence after the hardware scroll.
        write!(self.dest, "\x1b[{};{}r", hint.top + 1, hint.bottom + 1)?;
        let abs_delta = hint.delta.unsigned_abs();
        if hint.delta > 0 {
            write!(self.dest, "\x1b[{abs_delta}S")?;
        } else {
            write!(self.dest, "\x1b[{abs_delta}T")?;
        }
        self.dest.write_all(b"\x1b[r\x1b[H")?;
        Ok(())
    }

    fn inline_resize_requires_full_rewrite(&self) -> bool {
        if self.fullscreen {
            return false;
        }
        let (Some(prev), Some(next)) = (self.prev_size_on_write, self.size) else {
            return false;
        };

        // Mirrors Ink/log-update's resize guard: changing width invalidates
        // wrapping assumptions, and a shorter viewport can make previously
        // reachable rows disappear into scrollback. Reset the retained inline
        // canvas before diffing against the new terminal geometry.
        next.1 < prev.1 || (prev.0 != 0 && next.0 != prev.0)
    }

    fn inline_shrink_requires_full_rewrite(&self, prev_height: usize, new_height: usize) -> bool {
        if self.fullscreen || new_height >= prev_height {
            return false;
        }
        let Some((_cols, rows)) = self.size else {
            return false;
        };
        let rows = rows as usize;
        if rows == 0 {
            return false;
        }

        // Mirrors the main-screen Ink/log-update guard for shrinking from a
        // scrollback-producing frame to a frame that fits the viewport: rows
        // that should become visible again are in terminal scrollback and
        // cannot be pulled down with ordinary erase/cursor movement. Clear and
        // repaint instead of trying to patch the currently visible suffix.
        prev_height >= rows && new_height <= rows
    }

    fn inline_unreachable_rows_for_diff(&self, prev_height: usize) -> usize {
        if self.fullscreen {
            return 0;
        }
        let Some((_cols, rows)) = self.size else {
            return 0;
        };
        let rows = rows as usize;
        if rows == 0 || prev_height == 0 {
            return 0;
        }

        let viewport_y = prev_height.saturating_sub(rows);
        let cursor_restore_scroll = usize::from(prev_height >= rows);
        viewport_y + cursor_restore_scroll
    }

    fn inline_write_can_enter_pending_wrap(&self, rendered_width: usize) -> bool {
        !self.fullscreen
            && rendered_width > 0
            && self
                .size
                .is_some_and(|(cols, _)| cols > 0 && rendered_width >= cols as usize)
    }

    fn mark_inline_pending_wrap_after_full_write(&mut self, canvas: &Canvas) {
        let rendered_width = canvas
            .height()
            .checked_sub(1)
            .map(|last_row| canvas.ansi_row_rendered_width(last_row))
            .unwrap_or(0);
        self.inline_pending_wrap = self.inline_write_can_enter_pending_wrap(rendered_width);
    }

    fn mark_inline_pending_wrap_after_row_write(&mut self, canvas: &Canvas, y: usize) {
        let rendered_width = canvas.ansi_row_rendered_width(y);
        self.inline_pending_wrap = self.inline_write_can_enter_pending_wrap(rendered_width);
    }

    fn resolve_inline_pending_wrap(&mut self) -> io::Result<()> {
        if self.inline_pending_wrap {
            // Match Ink/log-update's handling of VT auto-wrap pending state: CR
            // returns to column 0 on the current row without advancing to the next
            // line, so subsequent relative movement starts from a known position.
            self.dest.write_all(b"\r")?;
            self.inline_pending_wrap = false;
        }
        Ok(())
    }

    fn move_inline_to_previous_line(&mut self, rows: u16) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        if rows > 0 {
            self.dest.queue(cursor::MoveToPreviousLine(rows))?;
        }
        Ok(())
    }

    fn move_inline_to_next_line(&mut self, rows: u16) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        if rows > 0 {
            self.dest.queue(cursor::MoveToNextLine(rows))?;
        }
        Ok(())
    }

    fn move_inline_to_column(&mut self, column: u16) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        self.dest.queue(cursor::MoveToColumn(column))?;
        Ok(())
    }

    fn write_inline_newline(&mut self) -> io::Result<()> {
        // CR is part of CRLF, so it resolves pending wrap and LF creates exactly
        // one new terminal row.
        self.dest.write_all(b"\r\n")?;
        self.inline_pending_wrap = false;
        Ok(())
    }

    fn write_inline_canvas_without_final_newline(&mut self, canvas: &Canvas) -> io::Result<()> {
        canvas.write_ansi_without_final_newline(&mut *self.dest)?;
        self.mark_inline_pending_wrap_after_full_write(canvas);
        Ok(())
    }

    fn write_inline_canvas_row_without_newline(
        &mut self,
        canvas: &Canvas,
        y: usize,
    ) -> io::Result<()> {
        canvas.write_ansi_row_without_newline(y, &mut *self.dest)?;
        self.mark_inline_pending_wrap_after_row_write(canvas, y);
        Ok(())
    }

    fn write_inline_canvas_row_from_col_without_newline(
        &mut self,
        canvas: &Canvas,
        y: usize,
        start_col: usize,
    ) -> io::Result<()> {
        canvas.write_ansi_row_from_col_without_newline(y, start_col, &mut *self.dest)?;
        self.mark_inline_pending_wrap_after_row_write(canvas, y);
        Ok(())
    }

    fn write_inline_full_repaint(&mut self, prev: &Canvas, canvas: &Canvas) -> io::Result<bool> {
        if self.fullscreen || prev.height() == 0 || canvas.height() == 0 {
            return Ok(false);
        }

        let prev_height = prev.height();
        let new_height = canvas.height();
        let max_height = prev_height.max(new_height);
        let unreachable_rows = self.inline_unreachable_rows_for_diff(prev_height);
        let mut current_y = prev_height.saturating_sub(1);

        for y in 0..max_height {
            if y < unreachable_rows {
                // Rows above this boundary are in scrollback or are protected by
                // Ink/log-update's cursor-restore-scroll guard. If their cells
                // changed, only a clear+rewrite can produce correct output. If
                // they are equal, skip them and still full-repaint every
                // reachable row below; this avoids duplicating tall transcripts
                // into native scrollback on ordinary layout-shift backstops.
                if !prev.row_eq(canvas, y) {
                    return Ok(false);
                }
                continue;
            }

            match y.cmp(&current_y) {
                std::cmp::Ordering::Less => {
                    self.move_inline_to_previous_line((current_y - y) as u16)?;
                }
                std::cmp::Ordering::Greater => {
                    let last_existing_line = prev_height.saturating_sub(1).max(current_y);
                    if y <= last_existing_line {
                        self.move_inline_to_next_line((y - current_y) as u16)?;
                    } else {
                        let move_to_last = last_existing_line.saturating_sub(current_y);
                        if move_to_last > 0 {
                            self.move_inline_to_next_line(move_to_last as u16)?;
                        }
                        let new_lines = y - last_existing_line;
                        for _ in 0..new_lines {
                            self.write_inline_newline()?;
                        }
                    }
                }
                std::cmp::Ordering::Equal => {
                    self.move_inline_to_column(0)?;
                }
            }
            current_y = y;

            if y < new_height {
                self.write_inline_canvas_row_without_newline(canvas, y)?;
            } else {
                self.dest
                    .queue(terminal::Clear(terminal::ClearType::CurrentLine))?;
                self.inline_pending_wrap = false;
            }
        }

        let target_y = new_height.saturating_sub(1);
        match target_y.cmp(&current_y) {
            std::cmp::Ordering::Greater => {
                self.move_inline_to_next_line((target_y - current_y) as u16)?;
            }
            std::cmp::Ordering::Less => {
                self.move_inline_to_previous_line((current_y - target_y) as u16)?;
            }
            std::cmp::Ordering::Equal => {}
        }

        Ok(true)
    }

    /// Undoes any cursor displacement left behind by a cursor declaration, returning
    /// the cursor to the canvas's last row. The inline row-diff logic relies on the
    /// cursor sitting there at the start of every write/clear.
    pub(super) fn restore_cursor_baseline(&mut self) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        if self.cursor_displacement_rows > 0 {
            self.dest
                .queue(cursor::MoveToNextLine(self.cursor_displacement_rows))?;
            self.cursor_displacement_rows = 0;
        }
        Ok(())
    }

    pub(super) fn reassert_keyboard_enhancement(&mut self) -> io::Result<()> {
        if self.enabled_keyboard_enhancement {
            // Match CC Ink's pop-before-push reassertion model for Kitty: if the
            // terminal preserved the stack, pop the old entry before pushing the
            // requested flags to avoid accumulating depth on every SIGCONT /
            // sleep-wake recovery. If the terminal reset the stack,
            // pop-on-empty is a no-op and the push restores depth 1.
            self.dest.execute(event::PopKeyboardEnhancementFlags)?;
            self.dest.execute(event::PushKeyboardEnhancementFlags(
                self.keyboard_enhancement_flags,
            ))?;
        }
        Ok(())
    }

    fn apply_raw_mode_enabled(&mut self, raw_mode_enabled: bool) -> io::Result<()> {
        if raw_mode_enabled != self.raw_mode_enabled {
            if raw_mode_enabled {
                if supports_extended_keys()
                    && terminal::supports_keyboard_enhancement().unwrap_or(false)
                {
                    self.dest.execute(event::PushKeyboardEnhancementFlags(
                        self.keyboard_enhancement_flags,
                    ))?;
                    self.enabled_keyboard_enhancement = true;
                }
                if self.mouse_capture {
                    self.dest.execute(event::EnableMouseCapture)?;
                }
                // Focus reporting lets components pause expensive work when the
                // terminal is blurred, mirroring CC Ink's TerminalFocusContext.
                self.dest.execute(event::EnableFocusChange)?;
                // Bracketed paste makes terminals deliver pasted text as a single
                // Event::Paste instead of a burst of key events. Terminals that don't
                // support it simply ignore the sequence.
                self.dest.execute(event::EnableBracketedPaste)?;
                terminal::enable_raw_mode()?;
            } else {
                terminal::disable_raw_mode()?;
                self.dest.execute(event::DisableBracketedPaste)?;
                self.dest.execute(event::DisableFocusChange)?;
                if self.mouse_capture {
                    self.dest.execute(event::DisableMouseCapture)?;
                }
                if self.enabled_keyboard_enhancement {
                    self.dest.execute(event::PopKeyboardEnhancementFlags)?;
                }
            }
            self.raw_mode_enabled = raw_mode_enabled;
        }
        Ok(())
    }
}

impl Drop for StdTerminal<'_> {
    fn drop(&mut self) {
        let _ = self.restore_cursor_baseline();
        let _ = self.apply_raw_mode_enabled(false);
        if self.fullscreen {
            let _ = self.dest.queue(terminal::LeaveAlternateScreen);
        } else if self.prev_canvas_height > 0 {
            let _ = self.dest.write_all(b"\r\n");
        }
        let _ = self.dest.queue(cursor::SetCursorStyle::DefaultUserShape);
        let _ = self.dest.execute(cursor::Show);
        unregister_terminal_for_panic_restore(self.fullscreen);
    }
}
