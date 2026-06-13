use crate::style::Color;
use crossterm::{
    csi,
    style::{Attribute, Colored},
};
use std::io::{self, Write};

pub(crate) fn sgr_reset(w: &mut impl Write) -> io::Result<()> {
    w.write_all(csi!("0m").as_bytes())
}

pub(crate) fn erase_to_eol(w: &mut impl Write) -> io::Result<()> {
    w.write_all(csi!("K").as_bytes())
}

pub(crate) fn sgr_attr(w: &mut impl Write, attr: Attribute) -> io::Result<()> {
    write!(w, csi!("{}m"), attr.sgr())
}

pub(crate) fn sgr_fg(w: &mut impl Write, color: Color) -> io::Result<()> {
    write!(w, csi!("{}m"), Colored::ForegroundColor(color))
}

pub(crate) fn sgr_bg(w: &mut impl Write, color: Color) -> io::Result<()> {
    write!(w, csi!("{}m"), Colored::BackgroundColor(color))
}

pub(crate) fn hyperlink_open(w: &mut impl Write, href: &str) -> io::Result<()> {
    write!(w, "\x1b]8;;{}\x1b\\", href)
}

pub(crate) fn hyperlink_close(w: &mut impl Write) -> io::Result<()> {
    w.write_all(b"\x1b]8;;\x1b\\")
}
