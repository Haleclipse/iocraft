use crate::ansi::{
    erase_to_eol, hyperlink_close, hyperlink_open, sgr_attr, sgr_bg, sgr_fg, sgr_reset,
    sgr_underline_color,
};
use crate::style::{Color, Weight};
use crossterm::style::Attribute;
use std::{
    collections::HashMap,
    fmt::{self, Display},
    io::{self, Write},
};
use unicode_segmentation::UnicodeSegmentation;

mod ansi;
mod measure_text;
mod output;
mod packed_screen;
mod screen;
mod search;
mod selection;
mod style_pool;

pub use measure_text::{
    expand_tabs, expand_tabs_with_interval, line_width, measure_text, widest_line, TextMeasurement,
};
pub(crate) use measure_text::{
    grapheme_width, handles_vs16_incorrectly, single_ascii_byte, skip_escape_sequence_graphemes,
    string_display_width, string_display_width_from_col,
};
pub use output::*;
pub use packed_screen::*;
pub use screen::*;
pub use selection::*;
pub use style_pool::*;

#[cfg(test)]
mod tests;
