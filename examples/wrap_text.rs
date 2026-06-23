//! Demonstrates `wrap_text`, the helper counterpart to the `Text` component's
//! wrapping/truncation modes.

use iocraft::prelude::*;

fn main() {
    let source = "  alpha beta gamma";

    println!("wrap:\n{}", wrap_text(source, 8, TextWrap::Wrap));
    println!("\nwrap-trim:\n{}", wrap_text(source, 8, TextWrap::WrapTrim));
    println!(
        "\ntruncate-middle:\n{}",
        wrap_text("abcdefghi", 6, TextWrap::TruncateMiddle)
    );
    println!(
        "\nlegacy middle (CC Ink no-op):\n{}",
        wrap_text("abcdefghi", 6, TextWrap::Middle)
    );
}
