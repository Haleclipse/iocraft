//! Demonstrates the opt-in RawAnsi line parse cache.
//!
//! This mirrors CC Ink's retained `Output.charCache` idea for callers that
//! repeatedly render pre-wrapped ANSI rows: unchanged lines reuse parsed style,
//! hyperlink, and bidi run metadata instead of reparsing every frame.

use iocraft::prelude::*;

fn main() {
    let mut cache = RawAnsiLineCache::new();
    let line = "\x1b[32;1msuccess\x1b[0m \x1b]8;;https://example.com\x07link\x1b]8;;\x07";

    for frame in 0..2 {
        let runs = cache.parse_line(line).to_vec();
        let entries = cache.len();
        println!(
            "frame {frame}: {} runs (cache entries: {entries})",
            runs.len()
        );
        for run in runs {
            println!(
                "  text={:?} color={:?} weight={:?} href={:?}",
                run.text, run.style.color, run.style.weight, run.hyperlink
            );
        }
    }
}
