use bitflags::bitflags;
use iocraft_macros::with_layout_style_props;
use taffy::{
    geometry,
    style::{Dimension, LengthPercentage, LengthPercentageAuto},
    Rect, Style,
};

// Re-export basic enum types.
pub use crossterm::style::Color;
pub use taffy::style::{
    AlignContent, AlignItems, Display, FlexDirection, FlexWrap, GridAutoFlow, JustifyContent,
    Overflow, Position,
};

/// Defines a type that represents a percentage [0.0-100.0] and is convertible to any of the
/// libary's other percent types. As a shorthand, you can express this in the
/// [`element!`](crate::element!) macro using the `pct` suffix, e.g. `50pct`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Percent(pub f32);

macro_rules! impl_from_length {
    ($name:ident) => {
        impl From<i16> for $name {
            fn from(l: i16) -> Self {
                $name::Length(l as _)
            }
        }
        impl From<i32> for $name {
            fn from(l: i32) -> Self {
                $name::Length(l as _)
            }
        }
        impl From<u16> for $name {
            fn from(l: u16) -> Self {
                $name::Length(l as _)
            }
        }
        impl From<u32> for $name {
            fn from(l: u32) -> Self {
                $name::Length(l as _)
            }
        }
    };
}

macro_rules! impl_from_percent {
    ($name:ident) => {
        impl From<Percent> for $name {
            fn from(p: Percent) -> Self {
                $name::Percent(p.0)
            }
        }
    };
}

macro_rules! new_length_percentage_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq)]
        pub enum $name {
            /// No padding.
            #[default]
            Unset,
            /// Sets an absolute value.
            Length(u32),
            /// Sets a percentage of the width or height of the parent.
            Percent(f32),
        }

        impl $name {
            fn or(self, other: Self) -> Self {
                match self {
                    $name::Unset => other,
                    _ => self,
                }
            }
        }

        impl From<$name> for LengthPercentage {
            fn from(p: $name) -> Self {
                match p {
                    $name::Unset => LengthPercentage::length(0.0),
                    $name::Length(l) => LengthPercentage::length(l as _),
                    $name::Percent(p) => LengthPercentage::percent(p / 100.0),
                }
            }
        }

        impl_from_length!($name);
        impl_from_percent!($name);
    }
}

new_length_percentage_type!(
    /// Defines the area to reserve around the element's content, but inside the border.
    ///
    /// See [the MDN documentation for padding](https://developer.mozilla.org/en-US/docs/Web/CSS/padding).
    Padding
);

new_length_percentage_type!(
    /// Defines the gaps in between rows or columns of flex items.
    ///
    /// See [the MDN documentation for gap](https://developer.mozilla.org/en-US/docs/Web/CSS/gap).
    Gap
);

macro_rules! new_size_type {
    ($(#[$m:meta])* $name:ident, $intrepr:ty, $def:expr) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq)]
        pub enum $name {
            /// The default behavior.
            #[default]
            Unset,
            /// Automatically selects a suitable size.
            Auto,
            /// Sets an absolute value.
            Length($intrepr),
            /// Sets a percentage of the width or height of the parent.
            Percent(f32),
        }

        impl $name {
            #[allow(dead_code)]
            fn or<T: Into<Self>>(self, other: T) -> Self {
                match self {
                    $name::Unset => other.into(),
                    _ => self,
                }
            }
        }

        impl From<$name> for LengthPercentageAuto {
            fn from(p: $name) -> Self {
                match p {
                    $name::Unset => $def.into(),
                    $name::Auto => LengthPercentageAuto::auto(),
                    $name::Length(l) => LengthPercentageAuto::length(l as _),
                    $name::Percent(p) => LengthPercentageAuto::percent(p / 100.0),
                }
            }
        }

        impl From<$name> for Dimension {
            fn from(p: $name) -> Self {
                match p {
                    $name::Unset => $def.into(),
                    $name::Auto => Dimension::auto(),
                    $name::Length(l) => Dimension::length(l as _),
                    $name::Percent(p) => Dimension::percent(p / 100.0),
                }
            }
        }

        impl_from_length!($name);
        impl_from_percent!($name);
    };
}

new_size_type!(
    /// Defines the area to reserve around the element's content, but outside the border.
    ///
    /// See [the MDN documentation for margin](https://developer.mozilla.org/en-US/docs/Web/CSS/margin).
    Margin,
    i32,
    Margin::Length(0)
);

new_size_type!(
    /// Defines a width or height of an element.
    Size,
    u32,
    Size::Auto
);

new_size_type!(
    /// Sets the position of a positioned element.
    ///
    /// See [the MDN documentation for inset](https://developer.mozilla.org/en-US/docs/Web/CSS/inset).
    Inset,
    i32,
    Size::Auto
);

/// Sets the initial main size of a flex item.
///
/// See [the MDN documentation for flex-basis](https://developer.mozilla.org/en-US/docs/Web/CSS/flex-basis).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum FlexBasis {
    /// Uses the value of the `width` or `height` property, or the content size if not set.
    #[default]
    Auto,
    /// Sets an absolute value.
    Length(u32),
    /// Sets a percentage of the width or height of the parent.
    Percent(f32),
}

impl From<FlexBasis> for Dimension {
    fn from(b: FlexBasis) -> Self {
        match b {
            FlexBasis::Auto => Dimension::auto(),
            FlexBasis::Length(l) => Dimension::length(l as _),
            FlexBasis::Percent(p) => Dimension::percent(p / 100.0),
        }
    }
}

/// A weight which can be applied to text.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Weight {
    /// The normal weight.
    #[default]
    Normal,
    /// The bold weight.
    Bold,
    /// The light weight.
    Light,
}

bitflags! {
    /// Defines the edges of an element, e.g. for border styling.
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Edges: u8 {
        /// The top edge.
        const Top = 0b00000001;
        /// The right edge.
        const Right = 0b00000010;
        /// The bottom edge.
        const Bottom = 0b00000100;
        /// The left edge.
        const Left = 0b00001000;
    }
}

/// Preprocesses a CSS-like track list so bare numbers are treated as cell counts.
///
/// iocraft's convention is that bare numbers mean terminal cells (`width: 12`), but the
/// CSS grammar taffy parses requires a unit (`12px`). We append `px` to bare numbers,
/// then undo the substitution for `repeat()` repetition counts, which must stay unitless.
fn css_with_cell_units(s: &str) -> String {
    // Append `px` to bare numbers. The optional prefix group catches numbers that are
    // part of an identifier (e.g. the `1` in a line name like `area1`), which must be
    // left untouched; the unit group catches numbers that already have one (`1fr`).
    let re = regex::Regex::new(r"([a-zA-Z_-]?)(\d+(?:\.\d+)?)([a-zA-Z%]*)").unwrap();
    let unitized = re.replace_all(s, |caps: &regex::Captures| {
        let (prefix, num, unit) = (&caps[1], &caps[2], &caps[3]);
        if prefix.is_empty() && unit.is_empty() {
            format!("{num}px")
        } else {
            format!("{prefix}{num}{unit}")
        }
    });
    // Undo unitization of repeat() repetition counts, which must stay unitless:
    // `repeat(3px,` → `repeat(3,`.
    let re2 = regex::Regex::new(r"repeat\(\s*(\d+)px\s*,").unwrap();
    re2.replace_all(&unitized, "repeat($1,").into_owned()
}

/// A grid track template, used for the `grid_template_columns` and `grid_template_rows`
/// props of grid containers.
///
/// Constructed from a CSS-like string. Bare numbers are interpreted as terminal cells:
///
/// ```
/// # use iocraft::GridTemplate;
/// let _ = GridTemplate::from("12 1fr auto");        // 12-cell sidebar, flexible main
/// let _ = GridTemplate::from("repeat(3, 1fr)");     // three equal columns
/// let _ = GridTemplate::from("minmax(10, 1fr) 2fr");
/// ```
///
/// # Panics
///
/// Conversion panics on invalid syntax, mirroring how invalid CSS is a programming error.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridTemplate {
    pub(crate) tracks: Vec<taffy::GridTemplateComponent<String>>,
    pub(crate) line_names: Vec<Vec<String>>,
}

// SAFETY: `GridTemplateComponent` is only !Send/!Sync because `CompactLength` can hold
// a type-erased calc() pointer. The only way to construct a `GridTemplate` is via the
// `From<&str>` parser below, and the CSS grammar taffy parses has no calc() support —
// so no pointer is ever stored. Same argument as `LayoutEngine` in render.rs; see
// <https://github.com/ccbrown/iocraft/issues/119>.
unsafe impl Send for GridTemplate {}
unsafe impl Sync for GridTemplate {}

impl From<&str> for GridTemplate {
    fn from(s: &str) -> Self {
        if s.trim().is_empty() {
            return Self::default();
        }
        let prepared = css_with_cell_units(s);
        let parsed: taffy::style::GridTemplateTracks<String, taffy::GridTemplateComponent<String>> =
            prepared
                .parse()
                .unwrap_or_else(|e| panic!("invalid grid template {s:?}: {e:?}"));
        Self {
            tracks: parsed.tracks,
            line_names: parsed.line_names,
        }
    }
}

impl From<String> for GridTemplate {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Implicit grid track sizes, used for the `grid_auto_columns` and `grid_auto_rows`
/// props of grid containers. Same syntax as [`GridTemplate`], minus `repeat()`.
///
/// # Panics
///
/// Conversion panics on invalid syntax.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridAutoTracks(pub(crate) Vec<taffy::TrackSizingFunction>);

// SAFETY: same argument as `GridTemplate` — values only originate from the calc-free
// CSS parser, so no `*const ()` is ever stored.
unsafe impl Send for GridAutoTracks {}
unsafe impl Sync for GridAutoTracks {}

impl From<&str> for GridAutoTracks {
    fn from(s: &str) -> Self {
        if s.trim().is_empty() {
            return Self::default();
        }
        let prepared = css_with_cell_units(s);
        let parsed: taffy::style::GridAutoTracks = prepared
            .parse()
            .unwrap_or_else(|e| panic!("invalid grid auto tracks {s:?}: {e:?}"));
        Self(parsed.0)
    }
}

impl From<String> for GridAutoTracks {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// A grid item placement, used for the `grid_column` and `grid_row` props.
///
/// Accepts CSS `grid-row`/`grid-column` syntax including the `start / end` shorthand:
///
/// ```
/// # use iocraft::GridPlacementSpec;
/// let _ = GridPlacementSpec::from("2");          // at line 2
/// let _ = GridPlacementSpec::from("1 / 3");      // from line 1 to line 3
/// let _ = GridPlacementSpec::from("span 2");     // span two tracks
/// let _ = GridPlacementSpec::from("1 / span 2"); // start at 1, span 2
/// let _ = GridPlacementSpec::from("header");     // named line/area
/// ```
///
/// # Panics
///
/// Conversion panics on invalid syntax.
#[derive(Clone, Debug, PartialEq)]
pub struct GridPlacementSpec {
    pub(crate) start: taffy::GridPlacement<String>,
    pub(crate) end: taffy::GridPlacement<String>,
}

impl Default for GridPlacementSpec {
    fn default() -> Self {
        Self {
            start: taffy::GridPlacement::Auto,
            end: taffy::GridPlacement::Auto,
        }
    }
}

fn parse_grid_placement(s: &str) -> taffy::GridPlacement<String> {
    s.trim()
        .parse()
        .unwrap_or_else(|e| panic!("invalid grid placement {s:?}: {e:?}"))
}

impl From<&str> for GridPlacementSpec {
    fn from(s: &str) -> Self {
        match s.split_once('/') {
            Some((start, end)) => Self {
                start: parse_grid_placement(start),
                end: parse_grid_placement(end),
            },
            None => {
                let start = parse_grid_placement(s);
                // CSS semantics: a single value sets the start; the end stays auto
                // (or spans, if the value itself is a span).
                Self {
                    start,
                    end: taffy::GridPlacement::Auto,
                }
            }
        }
    }
}

impl From<String> for GridPlacementSpec {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Named grid areas, used for the `grid_template_areas` prop of grid containers.
///
/// Each quoted row lists one area name per column track. Repeating a name over a
/// rectangular region merges it into a single area. `.` marks an unnamed cell:
///
/// ```
/// # use iocraft::GridAreas;
/// let _ = GridAreas::from(r#"
///     "header header"
///     "sidebar main"
///     "footer footer"
/// "#);
/// ```
///
/// # Panics
///
/// Conversion panics if rows have differing column counts or an area is not rectangular.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridAreas(pub(crate) Vec<taffy::GridTemplateArea<String>>);

impl From<&str> for GridAreas {
    fn from(s: &str) -> Self {
        // Extract quoted rows; if no quotes are present, treat each non-empty line as a row.
        let rows: Vec<Vec<&str>> = if s.contains('"') {
            s.split('"')
                .enumerate()
                .filter_map(|(i, part)| (i % 2 == 1).then_some(part))
                .map(|row| row.split_whitespace().collect())
                .collect()
        } else {
            s.lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .map(|row| row.split_whitespace().collect())
                .collect()
        };
        if rows.is_empty() {
            return Self::default();
        }
        let columns = rows[0].len();
        assert!(
            rows.iter().all(|r| r.len() == columns),
            "grid_template_areas rows must all have the same number of columns: {s:?}"
        );

        // Collect the bounding box of each named area. Grid coordinates are 1-based lines.
        let mut areas: Vec<taffy::GridTemplateArea<String>> = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            for (c, name) in row.iter().enumerate() {
                if *name == "." {
                    continue;
                }
                match areas.iter_mut().find(|a| a.name == *name) {
                    Some(area) => {
                        area.row_start = area.row_start.min(r as u16 + 1);
                        area.row_end = area.row_end.max(r as u16 + 2);
                        area.column_start = area.column_start.min(c as u16 + 1);
                        area.column_end = area.column_end.max(c as u16 + 2);
                    }
                    None => areas.push(taffy::GridTemplateArea {
                        name: name.to_string(),
                        row_start: r as u16 + 1,
                        row_end: r as u16 + 2,
                        column_start: c as u16 + 1,
                        column_end: c as u16 + 2,
                    }),
                }
            }
        }

        // Validate rectangularity: every cell within an area's bounding box must
        // carry that area's name.
        for area in &areas {
            for r in (area.row_start - 1)..(area.row_end - 1) {
                for c in (area.column_start - 1)..(area.column_end - 1) {
                    assert!(
                        rows[r as usize][c as usize] == area.name,
                        "grid area {:?} is not rectangular in {s:?}",
                        area.name
                    );
                }
            }
        }

        Self(areas)
    }
}

impl From<String> for GridAreas {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// A named grid area reference, used for the `grid_area` prop of grid items.
/// Equivalent to setting both `grid_row` and `grid_column` to the area name.
///
/// # Example
///
/// ```
/// # use iocraft::GridArea;
/// let _ = GridArea::from("header");
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridArea(pub(crate) Option<String>);

impl From<&str> for GridArea {
    fn from(s: &str) -> Self {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            Self(None)
        } else {
            Self(Some(trimmed.to_string()))
        }
    }
}

impl From<String> for GridArea {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

#[doc(hidden)]
#[with_layout_style_props]
#[non_exhaustive]
#[derive(Default)]
pub struct LayoutStyle {
    // fields added by proc macro, defined in ../macros/src/lib.rs
}

impl From<LayoutStyle> for Style {
    fn from(s: LayoutStyle) -> Self {
        // `grid_area: "name"` is shorthand for placing both axes at the named lines;
        // when set, it takes precedence over `grid_row` / `grid_column`.
        let (grid_row, grid_column) = match s.grid_area.0 {
            Some(name) => {
                // Index 0 means "no explicit ordinal", matching what taffy's own
                // CSS parser produces for a bare named line.
                let placement = || taffy::GridPlacement::NamedLine(name.clone(), 0);
                (
                    taffy::Line {
                        start: placement(),
                        end: placement(),
                    },
                    taffy::Line {
                        start: placement(),
                        end: placement(),
                    },
                )
            }
            None => (
                taffy::Line {
                    start: s.grid_row.start,
                    end: s.grid_row.end,
                },
                taffy::Line {
                    start: s.grid_column.start,
                    end: s.grid_column.end,
                },
            ),
        };
        Self {
            display: s.display,
            grid_template_columns: s.grid_template_columns.tracks,
            grid_template_column_names: s.grid_template_columns.line_names,
            grid_template_rows: s.grid_template_rows.tracks,
            grid_template_row_names: s.grid_template_rows.line_names,
            grid_auto_columns: s.grid_auto_columns.0,
            grid_auto_rows: s.grid_auto_rows.0,
            grid_auto_flow: s.grid_auto_flow,
            grid_template_areas: s.grid_template_areas.0,
            grid_row,
            grid_column,
            size: geometry::Size {
                width: s.width.into(),
                height: s.height.into(),
            },
            min_size: geometry::Size {
                width: s.min_width.into(),
                height: s.min_height.into(),
            },
            max_size: geometry::Size {
                width: s.max_width.into(),
                height: s.max_height.into(),
            },
            gap: geometry::Size {
                width: s.gap.or(s.column_gap).into(),
                height: s.gap.or(s.row_gap).into(),
            },
            padding: Rect {
                left: s.padding_left.or(s.padding).into(),
                right: s.padding_right.or(s.padding).into(),
                top: s.padding_top.or(s.padding).into(),
                bottom: s.padding_bottom.or(s.padding).into(),
            },
            margin: Rect {
                left: s.margin_left.or(s.margin).into(),
                right: s.margin_right.or(s.margin).into(),
                top: s.margin_top.or(s.margin).into(),
                bottom: s.margin_bottom.or(s.margin).into(),
            },
            inset: Rect {
                left: s.left.or(s.inset).into(),
                right: s.right.or(s.inset).into(),
                top: s.top.or(s.inset).into(),
                bottom: s.bottom.or(s.inset).into(),
            },
            overflow: geometry::Point {
                x: s.overflow_x.or(s.overflow).unwrap_or_default(),
                y: s.overflow_y.or(s.overflow).unwrap_or_default(),
            },
            position: s.position,
            flex_direction: s.flex_direction,
            flex_wrap: s.flex_wrap,
            flex_basis: s.flex_basis.into(),
            flex_grow: s.flex_grow,
            flex_shrink: s.flex_shrink.unwrap_or(1.0),
            align_items: s.align_items,
            align_content: s.align_content,
            justify_content: s.justify_content,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::prelude::TaffyGridLine;

    #[test]
    fn test_css_with_cell_units() {
        // Bare numbers gain px; existing units are untouched.
        assert_eq!(css_with_cell_units("12 1fr auto"), "12px 1fr auto");
        assert_eq!(css_with_cell_units("100px 50% 2fr"), "100px 50% 2fr");
        assert_eq!(css_with_cell_units("minmax(10, 1fr)"), "minmax(10px, 1fr)");
        // repeat() repetition counts must stay unitless.
        assert_eq!(css_with_cell_units("repeat(3, 12)"), "repeat(3, 12px)");
        assert_eq!(
            css_with_cell_units("repeat(auto-fill, 8)"),
            "repeat(auto-fill, 8px)"
        );
        // Numbers that are part of identifiers are untouched.
        assert_eq!(
            css_with_cell_units("[area1-start] 1fr"),
            "[area1-start] 1fr"
        );
        assert_eq!(css_with_cell_units("1.5 2"), "1.5px 2px");
    }

    #[test]
    fn test_grid_template_parses_tracks() {
        let t = GridTemplate::from("12 1fr auto");
        assert_eq!(t.tracks.len(), 3);
        let t = GridTemplate::from("repeat(3, 1fr)");
        assert_eq!(t.tracks.len(), 1); // one Repeat component
        assert_eq!(GridTemplate::from(""), GridTemplate::default());
    }

    #[test]
    #[should_panic(expected = "invalid grid template")]
    fn test_grid_template_invalid_panics() {
        let _ = GridTemplate::from("not!! valid@@");
    }

    #[test]
    fn test_grid_auto_tracks() {
        let a = GridAutoTracks::from("1fr 2fr");
        assert_eq!(a.0.len(), 2);
    }

    #[test]
    fn test_grid_placement_spec() {
        // Single line index.
        let p = GridPlacementSpec::from("2");
        assert_eq!(p.start, taffy::GridPlacement::from_line_index(2));
        assert_eq!(p.end, taffy::GridPlacement::Auto);
        // Start / end shorthand.
        let p = GridPlacementSpec::from("1 / 3");
        assert_eq!(p.start, taffy::GridPlacement::from_line_index(1));
        assert_eq!(p.end, taffy::GridPlacement::from_line_index(3));
        // Span.
        let p = GridPlacementSpec::from("span 2");
        assert_eq!(p.start, taffy::GridPlacement::Span(2));
        // Start with span end.
        let p = GridPlacementSpec::from("1 / span 2");
        assert_eq!(p.start, taffy::GridPlacement::from_line_index(1));
        assert_eq!(p.end, taffy::GridPlacement::Span(2));
        // Named line.
        let p = GridPlacementSpec::from("header");
        assert_eq!(
            p.start,
            taffy::GridPlacement::NamedLine("header".to_string(), 0)
        );
    }

    #[test]
    fn test_grid_areas_parsing_and_merging() {
        let areas = GridAreas::from(
            r#"
            "header header"
            "sidebar main"
            "footer footer"
        "#,
        );
        assert_eq!(areas.0.len(), 4);
        let header = areas.0.iter().find(|a| a.name == "header").unwrap();
        assert_eq!(
            (
                header.row_start,
                header.row_end,
                header.column_start,
                header.column_end
            ),
            (1, 2, 1, 3)
        );
        let main = areas.0.iter().find(|a| a.name == "main").unwrap();
        assert_eq!(
            (
                main.row_start,
                main.row_end,
                main.column_start,
                main.column_end
            ),
            (2, 3, 2, 3)
        );
    }

    #[test]
    fn test_grid_areas_dot_skips_cells() {
        let areas = GridAreas::from(r#""a ." ". b""#);
        assert_eq!(areas.0.len(), 2);
    }

    #[test]
    #[should_panic(expected = "same number of columns")]
    fn test_grid_areas_ragged_rows_panic() {
        let _ = GridAreas::from(r#""a a" "b""#);
    }

    #[test]
    #[should_panic(expected = "not rectangular")]
    fn test_grid_areas_non_rectangular_panics() {
        // "a" appears in an L shape.
        let _ = GridAreas::from(r#""a a" "a b""#);
    }
}

#[cfg(test)]
mod grid_layout_tests {
    use crate::prelude::*;

    /// End-to-end: a grid container with template areas lays children out at the
    /// expected canvas positions.
    #[test]
    fn test_grid_areas_layout() {
        let canvas = element! {
            View(
                display: Display::Grid,
                grid_template_columns: "8 1fr",
                grid_template_rows: "1 1fr 1",
                grid_template_areas: r#"
                    "header header"
                    "sidebar main"
                    "footer footer"
                "#,
                width: 24,
                height: 5,
            ) {
                View(grid_area: "header")  { Text(content: "HEAD") }
                View(grid_area: "sidebar") { Text(content: "SIDE") }
                View(grid_area: "main")    { Text(content: "MAIN") }
                View(grid_area: "footer")  { Text(content: "FOOT") }
            }
        }
        .render(Some(24));
        let s = canvas.to_string();
        let lines: Vec<&str> = s.lines().collect();
        // Row 0: header spans the full width, starting at column 0.
        assert!(lines[0].starts_with("HEAD"), "header row: {s:?}");
        // Rows 1-3: sidebar occupies the first 8 cells; main starts at column 8.
        assert!(lines[1].starts_with("SIDE"), "sidebar row: {s:?}");
        assert_eq!(&lines[1][8..12], "MAIN", "main column: {s:?}");
        // Last row: footer back at column 0.
        assert!(lines[4].starts_with("FOOT"), "footer row: {s:?}");
    }

    /// Line-index and span placement also work without named areas.
    #[test]
    fn test_grid_line_placement_layout() {
        let canvas = element! {
            View(
                display: Display::Grid,
                grid_template_columns: "repeat(3, 4)",
                grid_template_rows: "1",
                width: 12,
                height: 1,
            ) {
                // Out-of-order declaration with explicit rows: CSS sparse
                // auto-placement would otherwise push A to an implicit row,
                // because the placement cursor doesn't move backwards.
                View(grid_column: "2", grid_row: "1") { Text(content: "B") }
                View(grid_column: "1", grid_row: "1") { Text(content: "A") }
                View(grid_column: "3", grid_row: "1") { Text(content: "C") }
            }
        }
        .render(Some(12));
        let s = canvas.to_string();
        let line = s.lines().next().unwrap();
        assert_eq!(line.chars().next(), Some('A'), "col 1: {s:?}");
        assert_eq!(line.chars().nth(4), Some('B'), "col 2: {s:?}");
        assert_eq!(line.chars().nth(8), Some('C'), "col 3: {s:?}");
    }
}
