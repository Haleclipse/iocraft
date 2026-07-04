<div align="center">
  <h1><code>CometixTUI</code></h1>

  <p>
    <strong>Production-grade declarative TUI framework for Rust — ink philosophy, Rust strengths.</strong>
  </p>

  <p>
    Fork of <a href="https://github.com/ccbrown/iocraft">iocraft</a> ·
    Upstream badges:
    <a href="https://docs.rs/iocraft/"><img src="https://img.shields.io/docsrs/iocraft" alt="docs.rs" /></a>
    <a href="https://crates.io/crates/iocraft"><img src="https://img.shields.io/crates/v/iocraft" alt="crates.io" /></a>
  </p>
</div>

CometixTUI is a maintained fork of [iocraft](https://github.com/ccbrown/iocraft) that follows [ink](https://github.com/vadimdemedes/ink)'s design philosophy (React-like hooks, declarative composition) while leveraging Rust's type system and performance. The internal crate name remains `iocraft` for upstream compatibility.

## Fork Enhancements vs Upstream

### Focus System

| Feature | Upstream | CometixTUI |
|---------|----------|------------|
| `use_focus` / `use_focus_manager` hooks | - | Declarative focus with auto-registration/RAII cleanup |
| `FocusScope` component | - | Focus boundary with `trap_keys` for nested focus groups |
| UI-order tracking | - | Focus order matches render tree, not registration order |

### Event System

| Feature | Upstream | CometixTUI |
|---------|----------|------------|
| Event propagation | Broadcast to all | `SharedEventState` + `stop_propagation()` for bubble semantics |
| Ctrl+C handling | Hard intercept, no component access | Dispatched to component tree first; `stop_propagation()` suppresses default exit |
| Bracketed paste | - | `TerminalEvent::Paste(String)` for multi-char paste as single event |
| `use_keybinding` hook | - | Declarative keyboard bindings with modifier-aware parser |

### Rendering & Canvas

| Feature | Upstream | CometixTUI |
|---------|----------|------------|
| Post-render style overlay | - | `StyleOverlay` for cursor/selection/search without component awareness |
| CSI K attribute bleed fix | Kitty renders stale SGR across erased area | Full SGR reset before erase; single row-end exit path |
| Grapheme-cluster width | `UnicodeWidthStr` (inaccurate for emoji) | Per-cluster measurement matching npm `string-width` |
| CellWidth tracking | - | `Normal` / `Wide` / `WidthTail` for CJK/emoji alignment |
| Frame throttling | - | Configurable max FPS with coalescing (default 60fps) |
| ANSI helpers module | Raw `csi!` writes scattered | `ansi.rs` with semantic functions (`sgr_reset`, `erase_to_eol`, etc.) |
| Tmux truecolor clamp | - | Auto-downgrade RGB→ANSI-256 inside tmux (CC Ink parity) |
| OSC 8 hyperlink detection | - | `supports_hyperlinks()` with CC Ink terminal allowlist |

### Terminal

| Feature | Upstream | CometixTUI |
|---------|----------|------------|
| Suspend/resume | Terminal breaks after Ctrl+Z | SIGCONT listener with full reinitialize + redraw |
| Panic recovery | Terminal left in raw mode | Panic hook restores terminal state before output |
| Kitty keyboard protocol | - | `SystemContext::set_keyboard_enhancement_flags` |
| Terminal title | - | `SystemContext::set_terminal_title` (OSC 0) |
| Physical cursor control | Always shown | `CursorDeclaration { visible }` — ink model (overlay) or ratatui model (native cursor) |

### Components

| Feature | Upstream | CometixTUI |
|---------|----------|------------|
| `TextInput` cursor modes | SGR Reverse only | `physical_cursor` / `cursor_color` / `cursor_background_color` props |
| `Checkbox` | - | Toggle component with customizable indicators |
| `StaticOutput` | - | ink's `<Static>` — render-once content that exits the render loop |
| `ErrorBoundary` | - | `catch_unwind` wrapper with silent panic hook |
| OSC 8 hyperlinks | - | `Text(href: ...)` for clickable links in supporting terminals |

### Layout

| Feature | Upstream | CometixTUI |
|---------|----------|------------|
| Taffy version | 0.5.2 | 0.12.1 (flexbox + grid + layout-cache soundness fix) |
| CSS Grid DSL | - | `grid_template_areas`, `grid_template_columns/rows` with CSS string parsing |

### Context System

| Feature | Upstream | CometixTUI |
|---------|----------|------------|
| Context lookup correctness | Falls through borrowed inner to outer of same type | TypeId-indexed stack; borrowed = `None`, not fallback |

### Dependencies

| Dep | Upstream | CometixTUI |
|-----|----------|------------|
| taffy | 0.5.2 | 0.12.1 |
| crossterm | 0.28 | 0.29 |

## Getting Started

The internal API is `iocraft` — all code examples use `use iocraft::prelude::*`:

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

Built on [iocraft](https://github.com/ccbrown/iocraft) by [ccbrown](https://github.com/ccbrown). Design informed by [ink](https://github.com/vadimdemedes/ink) and [ratatui](https://github.com/ratatui/ratatui).
