<div align="center">
  <h1><code>CometixTUI</code></h1>

  <p>
    <strong>Production-grade declarative TUI framework for Rust — ink philosophy, Rust strengths.</strong>
  </p>

  <p>
    Fork of <a href="https://github.com/ccbrown/iocraft">iocraft</a> ·
    Internal crate: <code>iocraft</code>
  </p>
</div>

CometixTUI is a maintained fork of [iocraft](https://github.com/ccbrown/iocraft) that brings [Claude Code's ink fork](https://github.com/Haleclipse/ClaudeCodeRev) design patterns into Rust: React-like hooks, declarative composition, fullscreen selection/scroll/search, retained rendering, and terminal-native capabilities — while preserving Rust's type safety and zero-cost abstractions.

The internal crate name remains `iocraft` for upstream compatibility.

## Screenshots

<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/table.png" height=402 />
<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/form.png" height=387 />
<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/calculator.png" height=450 />
<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/weather-powershell.png" height=350 />

## What's Different from Upstream

CometixTUI adds **59,600+ lines** across **104 source files** and **114 examples**, organized into the following capability areas.

---

### Focus System

Declarative focus management ported from ink's `useFocus` / `useFocusManager`:

- **`use_focus` / `use_focus_manager` hooks** — auto-registration with RAII cleanup, `is_focused` / `focus()` / `blur()` API
- **`FocusScope` component** — focus boundary with `trap_keys` for nested groups (Tab/Shift+Tab cycling)
- **UI-order tracking** — focus order follows the render tree, not registration order
- **CC Ink focus parity** — focus-stack restoration, `view_tab_index` traversal, `focus_restore` semantics

### Event & Input System

Full event propagation with bubble semantics, matching ink's event model:

- **`SharedEventState` + `stop_propagation()`** — depth-first poll order = bubble order; any component can consume events
- **Ctrl+C dispatched to components** — framework delivers Ctrl+C as a normal `KeyEvent` before default exit; `stop_propagation()` suppresses quit (enables confirmation dialogs, save-before-exit)
- **Bracketed paste** — `TerminalEvent::Paste(String)` for multi-character paste as a single atomic event
- **`use_keybinding` hook** — declarative keyboard bindings with modifier-aware parser (`"ctrl+shift+s"`)
- **`use_input` / `use_input_event`** — CC Ink-style input callbacks with propagation-aware variants
- **View-level event handlers** — mouse, focus, key, paste capture/bubble handlers with CC Ink `event_type`/`bubbles`/`cancelable`/`defaultPrevented` metadata

### Canvas & Rendering

Terminal rendering engine extended with CC Ink's retained screen-buffer architecture:

- **Post-render `StyleOverlay`** — cursor inversion, search highlighting, selection rendering applied *after* the component tree draws, so components don't need awareness of overlays
- **CSI K attribute bleed fix** — full SGR reset before erase-to-EOL; single row-end exit path (fixes kitty rendering)
- **Grapheme-cluster width measurement** — per-cluster calculation matching npm `string-width` (accurate for CJK, ZWJ emoji, VS16)
- **`CellWidth` tracking** — `Normal` / `Wide` / `WidthTail` enum for correct CJK/emoji alignment
- **ANSI helpers module** (`ansi.rs`) — semantic functions replacing raw escape writes; tmux truecolor auto-clamp (RGB→ANSI-256); OSC 8 hyperlink detection with CC Ink terminal allowlist
- **Retained screen primitives** — per-cell canvas diffing, damage regions, clear/blit helpers, soft-wrap metadata, noSelect metadata, packed canvas snapshot/output-queue
- **Bidi-aware styled-run writes** — right-to-left text support in canvas rendering
- **Frame throttling** — configurable max FPS with event coalescing (default 60fps)

### Terminal Management

Robust terminal lifecycle management ported from CC Ink's terminal layer:

- **Suspend/resume (SIGCONT)** — self-healing redraw after Ctrl+Z; raw mode + keyboard enhancement auto-restore
- **Panic recovery** — panic hook restores terminal state (raw mode, alternate screen) before printing the panic message
- **Kitty keyboard protocol** — `SystemContext::set_keyboard_enhancement_flags` for disambiguating escape codes
- **Terminal title** — `SystemContext::set_terminal_title` (OSC 0)
- **Dual cursor mode** — `CursorDeclaration { visible }`: ink model (overlay cursor, physical hidden for IME only) or ratatui model (native terminal cursor)
- **Inline diff safety** — bounded diff planning scans, terminal height-aware main-screen layout
- **Fullscreen patch planners** — DECSTBM scroll-hint patch planning/serialization, cursor anchor/park patch planning, stateful retained-canvas frame planners
- **Raw input backend** — opt-in raw-stdin frontend bridge, async-reader bridge, terminal-side mode sequence helpers, scoped session event stream with tmux-compatible `modifyOtherKeys`
- **Terminal input tokenizer** — raw terminal byte normalization, tokenization, key/response/paste/mouse parsing, incomplete-sequence flush timing

### Components

New components ported from or inspired by CC Ink:

| Component | Description |
|-----------|-------------|
| `TextInput` | Extended with `cursor_color`, `cursor_background_color`, `physical_cursor` props for cross-theme visibility |
| `Checkbox` | Toggle component with customizable indicators |
| `StaticOutput` | ink's `<Static>` — render-once content that exits the render loop |
| `ErrorBoundary` | `catch_unwind` wrapper with silent panic hook for graceful error UI |
| `ErrorOverview` | Structured error display component |
| `ScrollBox` | CC Ink-style fullscreen scroll containers with virtual scroll, fast-path retained scroll blits |
| `ScrollView` | Enhanced with scroll-drain helpers, viewport child culling/cache planning, sticky follow-scroll |
| `AlternateScreen` | Dynamic alternate-screen mounting |
| `Button` | CC Ink-style tabIndex defaults, actions/state |
| `Link` | OSC 8 hyperlink component with `supports_hyperlinks()` detection |
| `Ansi` / `RawAnsi` | Pre-wrapped ANSI screen-buffer rendering with opt-in line parse caching |
| `Spacer` / `Newline` | Layout primitives |
| `NoSelect` | Gutter metadata for selection-aware rendering |
| `CachedSubtree` | Explicit clean-subtree retained canvas blits |
| `OffscreenFreeze` | Terminal viewport visibility tracking with opt-in subtree freeze |
| `Memo` | Transparent memoization wrapper for retained layout nodes |
| `Fragment` | Enhanced with topmost hit-testing |
| `NotificationProvider` | CC-style toast notification viewport |
| `KeybindingProvider` | Action-based keybinding contexts with chord support |

### Hooks

New hooks matching CC Ink's hook API:

| Hook | Description |
|------|-------------|
| `use_focus` / `use_focus_manager` | Declarative focus with RAII lifecycle |
| `use_keybinding` | Modifier-aware key binding declarations |
| `use_input` / `use_input_event` | CC Ink-style input callbacks |
| `use_selection` | Fullscreen text selection with copy-on-select, drag autoscroll |
| `use_screen_overlays` | Selection/search/current overlay ordering |
| `use_search_highlight` | Rendered-screen search with current-match overlays |
| `use_terminal_events` | Extended with propagation-aware variant |
| `use_terminal_size` | Terminal size context syncing |
| `use_terminal_focus` | Terminal focus reporting |
| `use_terminal_title` | Declarative terminal title |
| `use_terminal_notification` | Side-band terminal notifications/progress |
| `use_terminal_viewport` | Viewport visibility tracking for offscreen subtree freezing |
| `use_stdin` | Stdin/raw-mode status |
| `use_app` | App lifecycle exit handles |
| `use_interval` / `use_animation_frame` | Timer hooks for animation |
| `use_declared_cursor` | Custom cursor declarations for IME/accessibility |
| `use_keybinding_registry` | Action-based keybinding contexts with chord sequences |

### Fullscreen & Selection

CC Ink's fullscreen capabilities ported to Rust:

- **Terminal text selection** — mouse drag selection with word/line granularity
- **Copy-on-select** — selection clipboard integration with toast notifications
- **Selection drag autoscroll** — automatic scrolling when dragging past viewport edges
- **Search/highlight overlays** — rendered-screen search with current-match tracking and overlay painting
- **Selection/search overlay ordering** — CC Ink-style multi-layer overlay composition
- **DECSTBM scroll hints** — fullscreen terminal scroll optimization
- **Retained rendering** — node-cache metadata, generation-stamped IDs, logical-key tree reconciliation, dirty-tree invalidation, blit/clear planning, sibling contamination planning, escaping absolute-descendant repair

### Layout

- **Taffy 0.12.1** (upstream: 0.5.2) — flexbox + grid + layout-cache soundness fix
- **CSS Grid DSL** — `grid_template_areas`, `grid_template_columns/rows` with CSS string parsing
- **CC Ink style shorthands** — additional layout style properties matching ink's API surface
- **Border text labels** — dashed/per-edge border styling, opaque overlays, absolute overlay clamping

### Correctness Fixes

- **Context fallback bug** — `ContextStack` now uses TypeId indexing; borrowed inner context returns `None` instead of falling through to outer context of the same type
- **CSI K rendering order** — character written before erase-to-EOL so the last cell retains its style
- **Full-width ANSI row fix** — prevents erasing last cell in full-width rows

### Performance

- **Opt-in retained optimization mode** — retained render with opt-in canvas diff planning
- **Canvas diff planning** — bounded diff iteration, terminal diff patch optimization
- **Frame profiling** — opt-in render diff scan timings, mock-terminal benchmark collection, renderer workload profiling, debug repaint visualization
- **Duplicate repaint skip** — preflight scan deduplication

## Getting Started

```rust
use iocraft::prelude::*;

fn main() {
    element! {
        View(
            border_style: BorderStyle::Round,
            border_color: Color::Blue,
        ) {
            Text(content: "Hello from CometixTUI!")
        }
    }
    .print();
}
```

114 examples are available in the [`examples/`](examples/) directory, covering focus management, event propagation, forms, fullscreen apps, selection, search, scroll containers, retained rendering, terminal capabilities, and more.

## Upstream Relationship

- **origin**: `Haleclipse/CometixTUI` (this fork)
- **upstream**: `ccbrown/iocraft` (original)

This fork is maintained independently. Changes are not submitted as PRs to upstream.

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Acknowledgements

Built on [iocraft](https://github.com/ccbrown/iocraft) by [ccbrown](https://github.com/ccbrown). Design informed by [ink](https://github.com/vadimdemedes/ink) (and [Claude Code Source](https://github.com/Haleclipse/ClaudeCodeRev)) and [ratatui](https://github.com/ratatui/ratatui).
