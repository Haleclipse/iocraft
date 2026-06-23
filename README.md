<div align="center">
  <h1><code>iocraft</code></h1>

  <p>
    <strong>✨ A Rust crate for beautiful, artisanally crafted CLIs, TUIs, and text-based IO. ✨</strong>
  </p>

  <p>
    <a href="https://github.com/ccbrown/iocraft/actions"><img src="https://img.shields.io/github/actions/workflow/status/ccbrown/iocraft/commit.yaml" alt="GitHub Actions Workflow Status" /></a>
    <a href="https://docs.rs/iocraft/"><img src="https://img.shields.io/docsrs/iocraft" alt="docs.rs" /></a>
    <a href="https://crates.io/crates/iocraft"><img src="https://img.shields.io/crates/v/iocraft" alt="crates.io" /></a>
    <a href="https://app.codecov.io/github/ccbrown/iocraft"><img src="https://img.shields.io/codecov/c/github/ccbrown/iocraft" alt="Codecov" /></a>
  </p>
</div>

`iocraft` is a library for crafting beautiful text output and interfaces for the terminal or
logs. It allows you to easily build complex layouts and interactive elements using a
declarative API.

## Features

- Define your UI using a clean, highly readable syntax.
- Organize your UI using flexbox layouts powered by [`taffy`](https://docs.rs/taffy/).
- Output colored and styled UIs to the terminal or ASCII output anywhere else.
- Create animated or interactive elements with event handling and hooks.
- Build fullscreen terminal applications with ease.
- Pass props and context by reference to avoid unnecessary cloning.
- Broad support for both Unix and Windows terminals so your UIs look great everywhere.

## Screen Mode Boundaries

CC Ink-compatible APIs are documented as either mode-neutral or fullscreen-only:

- **Mode-neutral / main-screen safe**: layout, text measurement/wrapping,
  borders, ANSI/RawAnsi parsing, OSC 8 link metadata, capability gates,
  terminal title/focus/size contexts, notifications, key input, terminal
  query/response routing, raw terminal byte normalization plus
  tokenization/key/response/paste/mouse parsing and incomplete-sequence flush
  timing, stdin/raw-mode status, opt-in frame profiling/debug repaint
  visualization, and opt-in lifecycle policies such as Ctrl+Z suspension.
- **Retained-canvas / mode-neutral**: damage regions, per-cell canvas diffing,
  clear/blit helpers, soft-wrap metadata, noSelect metadata, and
  search/selection overlay painting operate on the `Canvas` and can be used by
  custom renderers or tests without forcing an app into fullscreen.
- **Fullscreen-only**: features that depend on fullscreen terminal state or
  fullscreen mouse routing, including terminal text selection, copy-on-select,
  selection drag autoscroll, `FullscreenMouseEvent` blank-cell metadata, and
  DECSTBM scroll hints.
- **Optimization-only / opt-in**: retained subtree blits, ANSI style transition
  caching, packed canvas snapshot/output-queue/ANSI-row writer/cell-view/line-cache/bidi-aware styled-run writes/set-cell/visible-cell/row-change/reset/pool-migration/diff/style-overlay/selection-text/selection-overlay/word-line-selection/selection-state-controller/search-highlight/hyperlink-lookup/debug-repaint/style/noSelect/clear/blit/absolute-clear-guard/shift intern pools, absolute-clear blit guards, renderer node-cache metadata, retained node blit/clear planning,
  explicit layout-shift tracking, order-preserving retained dirty-tree traversal, sibling blit contamination planning, escaping absolute-descendant blit repair planning/application, scroll viewport child culling/cache
  planning, virtual-scroll range/clamp planning, render-time scroll-drain helpers, retained scroll blit/shift fast-path plans,
  fullscreen cursor anchor/park patch planning, fullscreen DECSTBM scroll-hint patch serialization/frame composition, stateful typed/packed fullscreen retained-canvas frame planners, terminal diff patch
  optimization/serialization, frame/inline-clear heuristics, synchronized
  output, and alternate-screen mounting should not change main-screen
  application policy; apps opt into them explicitly.

When porting CC Ink behavior, keep Claude Code application policies out of the
framework core unless they are exposed as opt-in building blocks or examples.
The comparison rubric in `docs/cc-ink-alignment-principles.md` explains when to
prefer direct parity, Rust-native adaptation, opt-in helpers, or app-layer
examples instead of structural cloning. The current difference review lives in
`docs/cc-ink-alignment-audit.md`.

## Getting Started

If you're familiar with React, you'll feel right at home with `iocraft`. It uses all the same
concepts, but is text-focused and made for Rust.

Here's your first `iocraft` program:

```rust
use iocraft::prelude::*;

fn main() {
    element! {
        View(
            border_style: BorderStyle::Round,
            border_color: Color::Blue,
        ) {
            Text(content: "Hello, world!")
        }
    }
    .print();
}
```

<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/hello-world.png" height=237 />

Your UI is composed primarily via the `element!` macro, which allows you to
declare your UI elements in a React/SwiftUI-like syntax.

`iocraft` provides a few built-in components, such as `View`, `Text`, and
`TextInput`, but you can also create your own using the `#[component]` macro.

For example, here's a custom component that uses a hook to display a counter
which increments every 100ms:

```rust
use iocraft::prelude::*;
use std::time::Duration;

#[component]
fn Counter(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut count = hooks.use_state(|| 0);

    hooks.use_future(async move {
        loop {
            smol::Timer::after(Duration::from_millis(100)).await;
            count += 1;
        }
    });

    element! {
        Text(color: Color::Blue, content: format!("counter: {}", count))
    }
}

fn main() {
    smol::block_on(element!(Counter).render_loop()).unwrap();
}
```

<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/counter.svg" />

## More Examples

There are many [examples on GitHub](https://github.com/ccbrown/iocraft/tree/main/examples) which
demonstrate various concepts such as tables, progress bars, fullscreen apps,
forms, and more! Retained screen-buffer primitives such as selection,
hyperlinks, noSelect gutters, soft-wrap copy, visual selection overlays, and
copy prompts are demonstrated interactively in `examples/fullscreen_selection.rs`.
CC Ink-style App-level fullscreen selection event wiring is shown in
`examples/fullscreen_selection_events.rs`.
App-level theme-colored post-render selection context overlays and simulated
copy-on-select are demonstrated in `examples/selection_context_overlay.rs`.
Selection external-store subscriptions are shown in
`examples/selection_subscribe.rs`; CC Ink-style selection tracking across keyboard scroll jumps is shown in
`examples/selection_scroll_tracking.rs`; sticky follow-scroll selection tracking is shown in
`examples/selection_follow_scroll.rs`; drag-to-scroll selection autoscroll is shown in
`examples/selection_drag_autoscroll.rs`. Rendered-screen search/current-match overlays are shown in
`examples/search_highlight_overlay.rs`; search external-store subscriptions are shown in
`examples/search_highlight_subscribe.rs`. CC Ink-style selection/search/current overlay ordering is shown in
`examples/screen_overlays.rs`. Terminal viewport visibility tracking
offscreen subtree freezing, and the CC Ink virtual-list freeze bypass are shown in
`examples/terminal_viewport.rs`.
Terminal size context syncing is shown in `examples/terminal_size.rs`.
Terminal focus reporting is shown in `examples/terminal_focus.rs`. View mouse
handlers, release-click events, local click coordinates, blank-cell metadata, and
resize events are shown in `examples/view_events.rs`; root/Fragment topmost hit-testing is shown in
`examples/view_fragment_hit_test.rs`; View focus/key/paste capture/bubble handlers, CC Ink-style
`event_type`/bubbles/cancelable/defaultPrevented metadata, view target metadata, and DOM-style
propagation control are shown in
`examples/view_focus_events.rs`; View tab-index traversal is shown in
`examples/view_tab_index.rs`. CC Ink-style focus-stack restoration is shown in
`examples/focus_restore.rs`. CC Ink-style `Button` tabIndex defaults plus actions/state are
shown in `examples/button.rs`. CC-style toast notification provider/viewport
components are shown in `examples/notifications.rs`; selection copy-toasts are
shown in `examples/selection_copy_notifications.rs`; side-band terminal
notifications/progress are shown in `examples/terminal_notifications.rs`;
action-based keybinding contexts/chords are shown in `examples/action_keybindings.rs`. App lifecycle
exit handles are shown in `examples/use_app.rs`. Stdin/raw-mode status is shown
in `examples/use_stdin.rs`. Input callbacks are shown in `examples/use_input.rs`;
propagation-aware input events are shown in `examples/use_input_event.rs`.
Raw terminal tokenizer/parser bridging for custom stdin frontends is shown in
`examples/terminal_input_tokenizer.rs`; the opt-in raw-stdin frontend bridge and
byte-stream event adapter, async-reader bridge, terminal-side mode sequence / RAII guard helpers, explicit raw-input session guard, and scoped session event stream (including opt-in OS raw mode and tmux-compatible `modifyOtherKeys`) are shown in `examples/terminal_raw_input_frontend.rs`,
`examples/terminal_raw_input_event_stream.rs`, `examples/terminal_raw_input_reader.rs`, and `examples/terminal_raw_input_mode.rs`; opt-in frame profiling,
mock-terminal benchmark collection, renderer workload profiling, and debug repaint visualization are shown in
`examples/frame_profile.rs`, `examples/frame_profile_mock_benchmark.rs`,
`examples/renderer_profile_workloads.rs`, and `examples/debug_repaint_overlay.rs`; opt-in ANSI style transition caching, row serialization caching, and packed canvas snapshot/output-queue/ANSI-row writer/cell-view/line-cache/bidi-aware styled-run writes/set-cell/visible-cell/row-change/reset/pool-migration/diff/style-overlay/selection-text/selection-overlay/word-line-selection/selection-state-controller/search-highlight/hyperlink-lookup/debug-repaint/style/noSelect/clear/blit/absolute-clear-guard/shift intern pools are
shown in `examples/canvas_style_transition_cache.rs`, `examples/canvas_ansi_row_cache.rs`, and `examples/canvas_packed_screen.rs`; CC Ink-style renderer node-cache metadata
is shown in `examples/renderer_node_cache.rs`; generation-stamped retained node IDs are shown in
`examples/renderer_node_generations.rs`; logical-key retained tree reconciliation is shown in
`examples/renderer_retained_tree_reconciler.rs`; order-preserving retained renderer dirty-tree invalidation is shown in
`examples/renderer_dirty_tree.rs`; combined retained dirty-tree/cache planning is shown in
`examples/renderer_retained_tree_state.rs`; retained node blit/clear planning and canvas-side blit/clear application
are shown in `examples/retained_node_render_plan.rs`, with stateful retained-frame cache/commit
planning in `examples/renderer_retained_frame_state.rs`; explicit layout-shift tracking
for custom retained renderers is shown in `examples/renderer_layout_shift_tracker.rs`;
sibling blit contamination planning is shown in `examples/retained_child_blit_plan.rs`; escaping
absolute-descendant blit repair planning/application is shown in `examples/escaping_absolute_descendant_blits.rs`;
scroll viewport child culling/cache planning is shown in `examples/scroll_viewport_child_plan.rs`;
retained canvas per-cell diffing is shown in `examples/canvas_diff.rs`; the absolute-clear
blit guard for custom retained renderers is shown in
`examples/absolute_clear_blit_guard.rs`; terminal patch optimization/serialization
plus frame/main-screen inline diff heuristics, typed/packed safety decisions, and clear-repaint fallback patches, fullscreen cursor anchor/park patch planning, typed/packed fullscreen retained-canvas diff patches, complete fullscreen canvas frame patch composition, and stateful typed/packed fullscreen retained-canvas frame planners for custom renderers are shown
in `examples/terminal_diff_optimizer.rs`; fullscreen/atomic-safe DECSTBM scroll-hint patch
planning and serialization are shown in `examples/terminal_scroll_hint_patch.rs`, with safe frame composition shown in `examples/terminal_diff_optimizer.rs`.
Dynamic alternate-screen mounting is shown in
`examples/alternate_screen.rs`. Interval/animation hooks are shown in
`examples/use_interval.rs`. CC Ink-style fullscreen scroll containers are shown
in `examples/scroll_box.rs`; render-time scroll draining is shown in
`examples/scroll_drain.rs`, with opt-in `ScrollBox` integration in
`examples/scroll_box_fast_drain.rs` and the `FastScrollBox` convenience wrapper in `examples/fast_scroll_box.rs`; the pure ScrollBox render-offset planner and Rust-native `scrollToElement` rect conversion helper are shown in
`examples/scroll_box_offset_plan.rs` and `examples/scroll_box_scroll_to_rect.rs`; CC Ink-style visual-only ScrollBox clamp behavior is shown in
`examples/scroll_box_visual_clamp.rs`; virtual-scroll range/clamp planning
is shown in `examples/virtual_scroll_plan.rs`, with visual-only clamp semantics that let the committed scroll target run ahead of the mounted range, plus stateful height-cache/resize-freeze
planning in `examples/virtual_scroll_state.rs`; retained scroll fast-path planning/canvas application/full-width scroll-hint bridge is shown in
`examples/scroll_fast_path_plan.rs`, with stateful retained scroll-frame planning, safety-gated combined canvas skeleton application, previous-canvas diff shifting, and fullscreen terminal frame patch composition bridge in
`examples/scroll_fast_path_frame_state.rs`; transcript/modal pager keys and wheel acceleration
extracted from `ScrollKeybindingHandler` are shown in `examples/scroll_box_modal_pager.rs`.
Explicit clean-subtree retained canvas blits are shown
in `examples/cached_subtree.rs`. Spacer/Newline layout primitives are shown in
`examples/layout_primitives.rs`; CC Ink-style border text labels, dashed/per-edge
border styling, opaque overlays, absolute overlay clamping, and zero-height
ghost-text protection are shown in `examples/border_text.rs`,
`examples/opaque_overlay.rs`, `examples/absolute_overlay_clamp.rs`, and
`examples/zero_height_overlap.rs`.
Declarative terminal title updates are shown
in `examples/terminal_title.rs`. Custom cursor
declarations for IME/accessibility anchoring are shown in
`examples/declared_cursor.rs`. OSC 8 hyperlink metadata is shown in
`examples/link.rs`. ANSI strings rendered as styled text segments are shown in
`examples/ansi.rs`; pre-wrapped ANSI screen-buffer rendering and opt-in RawAnsi line parse caching are shown in
`examples/raw_ansi.rs` and `examples/raw_ansi_line_cache.rs`. Text styling including background color, strikethrough,
and CC Ink-style truncation modes is shown in `examples/text_styles.rs`.
NoSelect gutter metadata is shown in
`examples/no_select.rs`. Damage and clear/blit regions are shown in
`examples/screen_buffer_regions.rs`. Off-terminal side rendering for exact
search positions is shown in `examples/render_to_screen.rs`.

<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/table.png" height=402 />
<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/form.png" height=387 />
<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/overlap.png" height=450 />
<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/calculator.png" height=450 />
<img src="https://raw.githubusercontent.com/ccbrown/iocraft/refs/heads/main/examples/images/weather-powershell.png" height=350 />

## Shoutouts

`iocraft` was inspired by [Dioxus](https://github.com/DioxusLabs/dioxus) and
[Ink](https://github.com/vadimdemedes/ink), which you should also check out,
especially if you're building graphical interfaces or interested in using
JavaScript/TypeScript.

You may also want to check out [Ratatui](https://github.com/ratatui/ratatui),
which serves a similar purpose with a less declarative API.

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
