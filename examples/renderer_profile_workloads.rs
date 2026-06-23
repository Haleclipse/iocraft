//! Deterministic renderer/profile workloads for CC Ink alignment decisions.
//!
//! This is not a real TTY benchmark. It is a stable mock-terminal harness you
//! can run in CI to compare renderer changes before deciding whether heavier CC
//! Ink optimizations (automatic subtree blits, packed screens, style/row/ANSI
//! line caches, or a FastScrollBox) are worth making default.

use futures::StreamExt;
use iocraft::prelude::*;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const TICKS: usize = 8;

#[component]
fn FixedRowsWorkload(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0usize);

    hooks.use_future(async move {
        for _ in 0..TICKS {
            smol::Timer::after(Duration::from_millis(1)).await;
            tick += 1;
        }
    });

    let tick_value = tick.get();
    if tick_value >= TICKS {
        system.exit();
    }

    element! {
        View(width: 60, height: 12, flex_direction: FlexDirection::Column) {
            #((0..12).map(|row| element! {
                Text(content: format!("fixed row {row:02} tick {tick_value}"))
            }))
        }
    }
}

#[component]
fn ScrollingTranscriptWorkload(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0usize);

    hooks.use_future(async move {
        for _ in 0..TICKS {
            smol::Timer::after(Duration::from_millis(1)).await;
            tick += 1;
        }
    });

    let tick_value = tick.get();
    if tick_value >= TICKS {
        system.exit();
    }

    let lines = (0..(40 + tick_value))
        .map(|i| format!("transcript line {i:03} tick {tick_value}"))
        .collect::<Vec<_>>()
        .join("\n");

    element! {
        View(width: 64, height: 10) {
            ScrollView(scrollbar: Some(false)) {
                Text(content: lines)
            }
        }
    }
}

#[component]
fn FastScrollBoxWorkload(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0usize);
    let mut queued_tick = hooks.use_state(|| 0usize);
    let mut handle = hooks.use_ref_default::<ScrollBoxHandle>();

    hooks.use_future(async move {
        for _ in 0..TICKS {
            smol::Timer::after(Duration::from_millis(1)).await;
            tick += 1;
        }
    });

    let tick_value = tick.get();
    if tick_value >= TICKS {
        system.exit();
    } else if tick_value > 0 && queued_tick.get() < tick_value {
        handle.write().scroll_by(18);
        queued_tick.set(tick_value);
    }

    let rows = (0..160)
        .map(|i| format!("fast scroll transcript row {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (top, pending) = {
        let handle = handle.read();
        (handle.get_scroll_top(), handle.get_pending_delta())
    };

    element! {
        View(width: 70, height: 10, flex_direction: FlexDirection::Column) {
            Text(content: format!("top={top} pending={pending} tick={tick_value}"))
            View(height: 9) {
                FastScrollBox(handle, wheel_acceleration: Some(false), scrollbar: Some(false)) {
                    Text(content: rows)
                }
            }
        }
    }
}

#[component]
fn StyledRunsWorkload(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0usize);

    hooks.use_future(async move {
        for _ in 0..TICKS {
            smol::Timer::after(Duration::from_millis(1)).await;
            tick += 1;
        }
    });

    let tick_value = tick.get();
    if tick_value >= TICKS {
        system.exit();
    }

    let runs = (0..24)
        .flat_map(|i| {
            [
                MixedTextContent::new(format!("{tick_value}:{i:02} ")).color(Color::Cyan),
                MixedTextContent::new("ok ")
                    .background_color(Color::DarkBlue)
                    .color(Color::White),
                MixedTextContent::new("warn ")
                    .color(Color::Yellow)
                    .weight(Weight::Bold),
                MixedTextContent::new("err ")
                    .color(Color::Red)
                    .strikethrough(),
            ]
        })
        .collect::<Vec<_>>();

    element! {
        View(width: 72, height: 8) {
            MixedText(contents: runs, wrap: TextWrap::Wrap)
        }
    }
}

fn run_profile<E>(name: &str, mut element: E)
where
    E: ElementExt,
{
    let stats = Arc::new(Mutex::new(RenderFrameProfileStats::default()));
    let stats_for_callback = stats.clone();

    let frames = smol::block_on(
        element
            .mock_terminal_render_loop_with_profile(MockTerminalConfig::default(), move |event| {
                stats_for_callback.lock().unwrap().record(&event);
            })
            .collect::<Vec<_>>(),
    );

    let stats = stats.lock().unwrap();
    println!(
        "{name}: frames={} canvases={} repaint_ratio={:.2} avg_frame={:?} avg_changed_cells={:.1} max_changed_cells={}",
        stats.frames,
        frames.len(),
        stats.repaint_ratio(),
        stats.average_duration(),
        stats.average_changed_cells(),
        stats.max_changed_cells,
    );
}

fn seeded_canvas(width: usize, height: usize, seed: usize) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    for row in 0..height {
        canvas.subview_mut(0, 0, 0, 0, width, height).set_text(
            0,
            row as isize,
            &format!(
                "row {row:03} seed {seed:03} {}",
                "x".repeat(width.saturating_sub(18))
            ),
            CanvasTextStyle::default(),
        );
    }
    canvas.clear_damage();
    canvas
}

fn run_scroll_fast_path_canvas_microbench() {
    let width = 80;
    let height = 24;
    let iterations = 500;
    let plan = plan_scroll_fast_path(
        CachedClearRegion {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        },
        3,
        [CachedClearRegion {
            x: 10,
            y: 10,
            width: 20,
            height: 2,
        }],
    )
    .expect("small full-width delta should plan a retained scroll fast path");
    let previous = seeded_canvas(width, height, 0);

    let fast_start = Instant::now();
    let mut fast_damage = 0usize;
    for _ in 0..iterations {
        let mut next = Canvas::new(width, height);
        apply_scroll_fast_path_to_canvas(&mut next, &previous, &plan);
        fast_damage += next
            .damage_region()
            .map(|region| region.width * region.height)
            .unwrap_or_default();
    }
    let fast_elapsed = fast_start.elapsed();

    let full_start = Instant::now();
    let mut full_damage = 0usize;
    for seed in 0..iterations {
        let next = seeded_canvas(width, height, seed);
        full_damage += next.diff(&previous).len();
    }
    let full_elapsed = full_start.elapsed();

    println!(
        "scroll-fast-path-canvas: iterations={iterations} fast_elapsed={fast_elapsed:?} full_render_diff_elapsed={full_elapsed:?} fast_damage={} full_damage={}",
        fast_damage, full_damage
    );
}

fn run_ansi_row_cache_microbench() {
    let canvas = seeded_canvas(80, 24, 7);
    let iterations = 10_000;

    let uncached_start = Instant::now();
    let mut uncached_bytes = 0usize;
    for _ in 0..iterations {
        let mut out = Vec::new();
        canvas.write_ansi(&mut out).unwrap();
        uncached_bytes += out.len();
    }
    let uncached_elapsed = uncached_start.elapsed();

    let cached_start = Instant::now();
    let mut cached_bytes = 0usize;
    let mut cache = CanvasAnsiRowCache::new();
    for _ in 0..iterations {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[0m");
        for row in 0..canvas.height() {
            cache.write_row(&canvas, row, &mut out).unwrap();
            out.extend_from_slice(b"\r\n");
        }
        cached_bytes += out.len();
    }
    let cached_elapsed = cached_start.elapsed();

    println!(
        "ansi-row-cache: iterations={iterations} rows={} cache_entries={} uncached_elapsed={uncached_elapsed:?} cached_elapsed={cached_elapsed:?} bytes_equal={}",
        canvas.height(),
        cache.len(),
        uncached_bytes == cached_bytes,
    );
}

fn run_raw_ansi_line_cache_microbench() {
    let line = concat!(
        "\x1b[32;1msuccess\x1b[0m ",
        "\x1b[33mwarn\x1b[0m ",
        "\x1b]8;;https://example.com\x07link\x1b]8;;\x07 ",
        "plain text with emoji ✅ and CJK 中"
    );
    let iterations = 10_000;

    let uncached_start = Instant::now();
    let mut uncached_runs = 0usize;
    let mut uncached = RawAnsiLineCache::with_max_entries(0);
    for _ in 0..iterations {
        uncached_runs += uncached.parse_line(line).len();
    }
    let uncached_elapsed = uncached_start.elapsed();

    let cached_start = Instant::now();
    let mut cached_runs = 0usize;
    let mut cache = RawAnsiLineCache::new();
    for _ in 0..iterations {
        cached_runs += cache.parse_line(line).len();
    }
    let cached_elapsed = cached_start.elapsed();

    println!(
        "raw-ansi-line-cache: iterations={iterations} cache_entries={} uncached_elapsed={uncached_elapsed:?} cached_elapsed={cached_elapsed:?} runs_equal={}",
        cache.len(),
        uncached_runs == cached_runs,
    );
}

fn run_style_transition_cache_microbench() {
    let normal = CanvasResolvedStyle::default();
    let mut accent_text = CanvasTextStyle::default();
    accent_text.color = Some(Color::Yellow);
    accent_text.weight = Weight::Bold;
    accent_text.underline = true;
    let accent = CanvasResolvedStyle {
        text: accent_text,
        background_color: Some(Color::DarkBlue),
    };

    let mut cache = CanvasStyleTransitionCache::new();
    let start = Instant::now();
    for _ in 0..10_000 {
        let _ = cache.transition(normal, accent);
        let _ = cache.transition(accent, normal);
    }
    println!(
        "style-transition-cache: pairs={} elapsed={:?}",
        cache.len(),
        start.elapsed(),
    );
}

fn run_packed_screen_microbench() {
    let mut canvas = seeded_canvas(80, 24, 11);
    canvas.mark_no_select_region(0, 0, 4, 24);
    canvas.mark_soft_wrap_continuation(3, 72);
    canvas.set_overlay(8, 4, StyleOverlay::selection_background(Color::Blue));
    let iterations = 10_000;

    let mut pools = CanvasPackedCellPools::new();
    let previous = canvas.pack_with(&mut pools);
    let mut changed_canvas = canvas.clone();
    changed_canvas.subview_mut(0, 0, 0, 0, 80, 24).set_text(
        12,
        4,
        "DIFF",
        CanvasTextStyle::default(),
    );
    changed_canvas.mark_damage(DamageRegion {
        x: 12,
        y: 4,
        width: 4,
        height: 1,
    });
    let changed = changed_canvas.pack_with(&mut pools);
    let diff_count = previous.diff(&changed).len();

    let start = Instant::now();
    let mut packed_cells = 0usize;
    let mut no_select_cells = 0usize;
    for _ in 0..iterations {
        let packed = canvas.pack_with(&mut pools);
        packed_cells += packed.cells.len();
        no_select_cells += packed.no_select.iter().filter(|marked| **marked).count();
    }
    println!(
        "packed-screen: iterations={iterations} elapsed={:?} cells={} no_select={} diff_count={} chars={} styles={} links={}",
        start.elapsed(),
        packed_cells,
        no_select_cells,
        diff_count,
        pools.char_len(),
        pools.style_len(),
        pools.hyperlink_len(),
    );
}

fn main() {
    run_profile("fixed-rows", element!(FixedRowsWorkload));
    run_profile(
        "scrolling-transcript",
        element!(ScrollingTranscriptWorkload),
    );
    run_profile("styled-runs", element!(StyledRunsWorkload));
    run_profile("fast-scroll-box", element!(FastScrollBoxWorkload));
    run_scroll_fast_path_canvas_microbench();
    run_ansi_row_cache_microbench();
    run_raw_ansi_line_cache_microbench();
    run_style_transition_cache_microbench();
    run_packed_screen_microbench();
}
