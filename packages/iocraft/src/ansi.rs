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
    w.write_all(b"\x1b]8;;")?;
    for ch in href.chars().filter(|ch| !ch.is_control()) {
        write!(w, "{ch}")?;
    }
    w.write_all(b"\x1b\\")
}

pub(crate) fn hyperlink_close(w: &mut impl Write) -> io::Result<()> {
    w.write_all(b"\x1b]8;;\x1b\\")
}

pub(crate) fn terminal_title(w: &mut (impl Write + ?Sized), title: &str) -> io::Result<()> {
    w.write_all(b"\x1b]0;")?;
    for ch in title.chars().filter(|ch| !ch.is_control()) {
        write!(w, "{ch}")?;
    }
    w.write_all(b"\x07")
}

#[cfg(test)]
mod tests {
    #[test]
    fn terminal_title_filters_control_chars() {
        let mut buf = Vec::new();
        super::terminal_title(&mut buf, "safe\x1b]2;owned\x07").unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "\x1b]0;safe]2;owned\x07");
        assert!(!output.contains("\x1b]2;owned"));
    }
}
