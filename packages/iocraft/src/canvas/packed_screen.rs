use super::*;

/// reused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineCluster {
    /// Grapheme text, including TAB/control/ESC graphemes that the writer may skip.
    pub text: String,
    /// Precomputed terminal display width of this grapheme cluster.
    pub width: usize,
}

/// One styled segment for packed direct line writes.
///
/// This is the Rust-native counterpart to one CC Ink styled text run after ANSI
/// tokenization and hyperlink extraction. The line writer interns the typed
/// style/link once for the run, then reuses [`CanvasPackedLineCache`] for the
/// expensive grapheme-width work inside `text`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineRun<'a> {
    /// Plain text for this run. ANSI/terminal control sequences are skipped by
    /// the packed writer, but callers that already parsed ANSI should pass only
    /// printable text here.
    pub text: &'a str,
    /// Resolved style applied to printable cells in this run.
    pub style: CanvasResolvedStyle,
    /// Optional OSC 8 hyperlink target for printable cells in this run.
    pub hyperlink: Option<&'a str>,
}

/// One pre-interned styled segment for packed direct line writes.
///
/// Use this variant when a custom renderer already owns stable packed style and
/// hyperlink IDs. The IDs must come from the same compatible
/// [`CanvasPackedCellPools`] used by the destination screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineRunIds<'a> {
    /// Plain text for this run.
    pub text: &'a str,
    /// Packed style ID from [`CanvasPackedCellPools::intern_style`].
    pub style_id: u32,
    /// Packed hyperlink ID from [`CanvasPackedCellPools::intern_hyperlink`].
    pub hyperlink_id: u32,
}

/// Owned styled segment used by [`CanvasPackedOutput`] write operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineOwnedRun {
    /// Plain text for this run.
    pub text: String,
    /// Resolved style applied to printable cells in this run.
    pub style: CanvasResolvedStyle,
    /// Optional OSC 8 hyperlink target for printable cells in this run.
    pub hyperlink: Option<String>,
}

impl<'a> From<CanvasPackedLineRun<'a>> for CanvasPackedLineOwnedRun {
    fn from(value: CanvasPackedLineRun<'a>) -> Self {
        Self {
            text: value.text.to_string(),
            style: value.style,
            hyperlink: value.hyperlink.map(str::to_string),
        }
    }
}

/// Opt-in cache for packed direct line writes.
#[derive(Clone, Debug)]
pub struct CanvasPackedLineCache {
    max_entries: usize,
    lines: HashMap<String, Vec<CanvasPackedLineCluster>>,
    scratch: Vec<CanvasPackedLineCluster>,
}

impl Default for CanvasPackedLineCache {
    fn default() -> Self {
        Self::with_max_entries(16_384)
    }
}

impl CanvasPackedLineCache {
    /// Creates a cache with the CC Ink `Output.charCache` reset cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a cache with an explicit maximum number of retained lines.
    ///
    /// `max_entries == 0` disables retention while still using the same parser
    /// path, useful for benchmarks and tests.
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            max_entries,
            lines: HashMap::new(),
            scratch: Vec::new(),
        }
    }

    /// Returns cached grapheme clusters for one logical line.
    pub fn clusters(&mut self, line: &str) -> &[CanvasPackedLineCluster] {
        if self.max_entries == 0 {
            self.scratch = build_canvas_packed_line_clusters(line);
            return &self.scratch;
        }

        if !self.lines.contains_key(line) && self.lines.len() >= self.max_entries {
            self.lines.clear();
        }
        self.lines
            .entry(line.to_string())
            .or_insert_with(|| build_canvas_packed_line_clusters(line))
            .as_slice()
    }

    /// Number of retained line entries.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the retained cache is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Clears retained line entries and scratch storage.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scratch.clear();
    }
}

fn build_canvas_packed_line_clusters(line: &str) -> Vec<CanvasPackedLineCluster> {
    line.graphemes(true)
        .map(|grapheme| CanvasPackedLineCluster {
            text: grapheme.to_string(),
            width: grapheme_width(grapheme),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CanvasPackedStyledLineCluster {
    text: String,
    width: usize,
    style_id: u32,
    hyperlink_id: u32,
}

fn skip_escape_sequence_packed_clusters(clusters: &[CanvasPackedLineCluster], idx: usize) -> usize {
    let Some(next) = clusters
        .get(idx + 1)
        .and_then(|cluster| single_ascii_byte(&cluster.text))
    else {
        return idx + 1;
    };

    match next {
        b'(' | b')' | b'*' | b'+' => (idx + 3).min(clusters.len()),
        b'[' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text)
                    .is_some_and(|byte| (0x40..=0x7e).contains(&byte))
                {
                    return j + 1;
                }
                j += 1;
            }
            clusters.len()
        }
        b']' | b'P' | b'_' | b'^' | b'X' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text) == Some(0x07) {
                    return j + 1;
                }
                if single_ascii_byte(&clusters[j].text) == Some(0x1b)
                    && clusters
                        .get(j + 1)
                        .and_then(|cluster| single_ascii_byte(&cluster.text))
                        == Some(b'\\')
                {
                    return (j + 2).min(clusters.len());
                }
                j += 1;
            }
            clusters.len()
        }
        0x30..=0x7e => (idx + 2).min(clusters.len()),
        _ => idx + 1,
    }
}

/// A partial style that can be overlaid on an already-rendered [`CanvasCell`] without
/// touching the original text or style. Each `None` field means "keep the original value";
/// `Some(v)` means "override with `v`".

/// Public width marker used by opt-in packed canvas snapshots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CanvasPackedCellWidth {
    /// A normal single-column character or blank cell.
    #[default]
    Normal,
    /// The first column of a double-width grapheme.
    Wide,
    /// The trailing cell occupied by a double-width grapheme.
    WidthTail,
    /// A skipped placeholder where a wide grapheme would have crossed the right edge.
    SpacerHead,
}

impl From<CellWidth> for CanvasPackedCellWidth {
    fn from(value: CellWidth) -> Self {
        match value {
            CellWidth::Normal => Self::Normal,
            CellWidth::Wide => Self::Wide,
            CellWidth::WidthTail => Self::WidthTail,
            CellWidth::SpacerHead => Self::SpacerHead,
        }
    }
}

/// Packed retained-canvas cell using interned IDs instead of owned strings/styles.
///
/// This is an opt-in, Rust-native counterpart to CC Ink's packed `Screen` cell
/// words: character, resolved style, hyperlink, and width metadata are stable
/// integer IDs allocated by [`CanvasPackedCellPools`]. It is intended for
/// custom renderers, profilers, and benchmarks; iocraft's default canvas remains
/// a typed, readable data structure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CanvasPackedCell {
    /// Interned character/string ID. Blank cells use the same ID as a literal space.
    pub char_id: u32,
    /// Interned resolved style ID, including post-render overlay and background.
    pub style_id: u32,
    /// Interned OSC 8 hyperlink ID. `0` means no hyperlink.
    pub hyperlink_id: u32,
    /// Cell-width marker used for wide grapheme and spacer handling.
    pub width: CanvasPackedCellWidth,
}

impl CanvasPackedCell {
    /// Returns whether this packed cell is visually empty under the pool defaults.
    pub fn is_empty(self) -> bool {
        self.char_id == 0
            && self.style_id == 0
            && self.hyperlink_id == 0
            && self.width == CanvasPackedCellWidth::Normal
    }
}

/// Resolved borrowed view of a packed cell.
///
/// CC Ink exposes object-shaped `Cell` values from `cellAt(...)` and
/// `visibleCellAtIndex(...)` even though the backing screen is packed integer
/// arrays. This view provides the same inspection shape for opt-in iocraft
/// packed screens without unpacking the whole buffer or changing the default
/// typed [`Canvas`] model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasPackedCellView<'a> {
    /// Interned character ID from the packed cell.
    pub char_id: u32,
    /// Character/grapheme string resolved through [`CanvasPackedCellPools`].
    pub character: &'a str,
    /// Interned style ID from the packed cell.
    pub style_id: u32,
    /// Typed resolved style, if the ID exists in the compatible pools.
    pub style: Option<CanvasResolvedStyle>,
    /// Interned hyperlink ID from the packed cell. `0` means no hyperlink.
    pub hyperlink_id: u32,
    /// OSC 8 hyperlink target resolved through [`CanvasPackedCellPools`].
    pub hyperlink: Option<&'a str>,
    /// Cell-width marker used for wide grapheme and spacer handling.
    pub width: CanvasPackedCellWidth,
}

/// Intern pools shared across opt-in packed canvas snapshots.
///
/// CC Ink keeps `CharPool`, `StylePool`, and `HyperlinkPool` beside its packed
/// `Screen` so blits and diffs can compare small integers instead of per-cell
/// strings and style objects. This helper exposes the same optimization shape
/// without changing [`Canvas`]'s default representation or terminal writer.
#[derive(Clone, Debug)]
pub struct CanvasPackedCellPools {
    chars: Vec<String>,
    char_ids: HashMap<String, u32>,
    ascii_char_ids: [u32; 128],
    hyperlinks: Vec<String>,
    hyperlink_ids: HashMap<String, u32>,
    styles: Vec<CanvasResolvedStyle>,
    style_ids: HashMap<CanvasResolvedStyle, u32>,
}

impl Default for CanvasPackedCellPools {
    fn default() -> Self {
        let mut pools = Self {
            chars: vec![" ".to_string(), String::new()],
            char_ids: HashMap::from([(String::new(), 1)]),
            ascii_char_ids: [u32::MAX; 128],
            hyperlinks: vec![String::new()],
            hyperlink_ids: HashMap::new(),
            styles: vec![CanvasResolvedStyle::default()],
            style_ids: HashMap::from([(CanvasResolvedStyle::default(), 0)]),
        };
        pools.ascii_char_ids[b' ' as usize] = 0;
        pools
    }
}

impl CanvasPackedCellPools {
    /// Creates empty pools containing only CC Ink-compatible default entries:
    /// character `" "` at ID 0, spacer `""` at ID 1, no hyperlink at ID 0,
    /// and default style at ID 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns a character/grapheme string and returns its stable pool ID.
    pub fn intern_char(&mut self, value: &str) -> u32 {
        if value.len() == 1 {
            let code = value.as_bytes()[0];
            if code < 128 {
                let cached = self.ascii_char_ids[code as usize];
                if cached != u32::MAX {
                    return cached;
                }
                let id = self.chars.len() as u32;
                self.chars.push(value.to_string());
                self.ascii_char_ids[code as usize] = id;
                return id;
            }
        }

        if let Some(id) = self.char_ids.get(value) {
            return *id;
        }
        let id = self.chars.len() as u32;
        self.chars.push(value.to_string());
        self.char_ids.insert(value.to_string(), id);
        id
    }

    /// Returns an interned character by ID.
    pub fn character(&self, id: u32) -> Option<&str> {
        self.chars.get(id as usize).map(String::as_str)
    }

    /// Interns an optional OSC 8 hyperlink and returns its stable pool ID.
    pub fn intern_hyperlink(&mut self, value: Option<&str>) -> u32 {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return 0;
        };
        if let Some(id) = self.hyperlink_ids.get(value) {
            return *id;
        }
        let id = self.hyperlinks.len() as u32;
        self.hyperlinks.push(value.to_string());
        self.hyperlink_ids.insert(value.to_string(), id);
        id
    }

    /// Returns an interned OSC 8 hyperlink by ID. ID 0 is `None`.
    pub fn hyperlink(&self, id: u32) -> Option<&str> {
        (id != 0)
            .then(|| self.hyperlinks.get(id as usize).map(String::as_str))
            .flatten()
    }

    /// Interns a resolved terminal style and returns its stable pool ID.
    pub fn intern_style(&mut self, style: CanvasResolvedStyle) -> u32 {
        if let Some(id) = self.style_ids.get(&style) {
            return *id;
        }
        let id = self.styles.len() as u32;
        self.styles.push(style);
        self.style_ids.insert(style, id);
        id
    }

    /// Returns an interned resolved terminal style by ID.
    pub fn style(&self, id: u32) -> Option<CanvasResolvedStyle> {
        self.styles.get(id as usize).copied()
    }

    /// Returns whether an interned style is visibly meaningful on a space cell.
    pub fn style_visible_on_space(&self, id: u32) -> bool {
        self.style(id)
            .map(CanvasResolvedStyle::is_visible_on_space)
            .unwrap_or(false)
    }

    /// Interns the result of applying a post-render overlay to an existing style ID.
    ///
    /// This is the typed packed-screen counterpart to CC Ink's `StylePool`
    /// overlay helpers. Unknown base IDs fall back to the default style, matching
    /// the forgiving lookup behavior of the JS pool's `get(id) ?? []` path.
    pub fn style_id_with_overlay(&mut self, base_id: u32, overlay: StyleOverlay) -> u32 {
        let base = self.style(base_id).unwrap_or_default();
        self.intern_style(base.with_overlay(overlay))
    }

    /// Interns an inverse-video variant of an existing style ID.
    pub fn style_id_with_inverse(&mut self, base_id: u32) -> u32 {
        self.style_id_with_overlay(base_id, StyleOverlay::inverse())
    }

    /// Interns a selection-background variant that preserves foreground styling.
    pub fn style_id_with_selection_background(&mut self, base_id: u32, color: Color) -> u32 {
        self.style_id_with_overlay(base_id, StyleOverlay::selection_background(color))
    }

    /// Interns a current-search-match variant with the CC Ink yellow/inverse marker.
    pub fn style_id_with_current_match(&mut self, base_id: u32, color: Color) -> u32 {
        self.style_id_with_overlay(base_id, StyleOverlay::current_match(color))
    }

    /// Resolves a packed cell into a borrowed object-shaped view.
    pub fn cell_view(&self, cell: CanvasPackedCell) -> CanvasPackedCellView<'_> {
        CanvasPackedCellView {
            char_id: cell.char_id,
            character: self.character(cell.char_id).unwrap_or(" "),
            style_id: cell.style_id,
            style: self.style(cell.style_id),
            hyperlink_id: cell.hyperlink_id,
            hyperlink: self.hyperlink(cell.hyperlink_id),
            width: cell.width,
        }
    }

    /// Returns whether a packed cell should be rendered by a sparse row writer.
    ///
    /// This mirrors CC Ink's `visibleCellAtIndex(...)` optimization: spacer
    /// cells and default blank spaces are skipped; foreground-only spaces are
    /// skipped once the same style is already the last rendered style on the
    /// row; spaces with hyperlinks or visible-on-space styles still render.
    pub fn cell_visible_for_sparse_render(
        &self,
        cell: CanvasPackedCell,
        last_rendered_style_id: Option<u32>,
    ) -> bool {
        if matches!(
            cell.width,
            CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
        ) || cell.char_id == 1
        {
            return false;
        }

        if cell.char_id == 0
            && cell.hyperlink_id == 0
            && !self.style_visible_on_space(cell.style_id)
            && (cell.style_id == 0 || last_rendered_style_id == Some(cell.style_id))
        {
            return false;
        }

        true
    }

    /// Interns one canvas cell plus any post-render overlay into a packed cell.
    pub fn intern_cell(
        &mut self,
        cell: &CanvasCell,
        overlay: Option<StyleOverlay>,
    ) -> CanvasPackedCell {
        let char_text = match cell.cell_width {
            CellWidth::WidthTail => "",
            CellWidth::SpacerHead => " ",
            CellWidth::Normal | CellWidth::Wide => cell.text().unwrap_or(" "),
        };
        let base_style = cell
            .character
            .as_ref()
            .map(|character| character.style)
            .unwrap_or_default();
        let base_background = cell.background_color;
        let resolved_style = match overlay {
            Some(overlay) => CanvasResolvedStyle {
                text: base_style.with_overlay(&overlay),
                background_color: overlay.background_color.unwrap_or(base_background),
            },
            None => CanvasResolvedStyle {
                text: base_style,
                background_color: base_background,
            },
        };

        CanvasPackedCell {
            char_id: self.intern_char(char_text),
            style_id: self.intern_style(resolved_style),
            hyperlink_id: self.intern_hyperlink(cell.hyperlink()),
            width: cell.cell_width.into(),
        }
    }

    /// Number of interned character strings.
    pub fn char_len(&self) -> usize {
        self.chars.len()
    }

    /// Number of interned hyperlinks, including the ID 0 empty entry.
    pub fn hyperlink_len(&self) -> usize {
        self.hyperlinks.len()
    }

    /// Number of interned resolved styles.
    pub fn style_len(&self) -> usize {
        self.styles.len()
    }

    /// Clears all pools back to their default entries.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Returns a new pool generation with char/hyperlink pools cleared and style IDs preserved.
    ///
    /// CC Ink keeps `StylePool` session-lived while `CharPool`/`HyperlinkPool`
    /// may be replaced between turns; old packed screens are then migrated with
    /// `migrateScreenPools(...)`. Because iocraft keeps these pools bundled for
    /// a Rust-friendly API, this helper creates that same generational shape:
    /// the returned pools have fresh transient char/link IDs but identical style
    /// IDs, so [`CanvasPackedScreen::migrate_transient_pools`] can re-intern a
    /// retained packed screen without invalidating style references.
    pub fn fork_with_transient_pools_cleared(&self) -> Self {
        let mut next = Self::default();
        next.styles = self.styles.clone();
        next.style_ids = self.style_ids.clone();
        next
    }

    /// Clears only char/hyperlink pools while preserving style IDs.
    ///
    /// Use this only after all screens that reference the old transient IDs have
    /// either been discarded or migrated with
    /// [`CanvasPackedScreen::migrate_transient_pools`]. For most retained
    /// double-buffering code, [`CanvasPackedCellPools::fork_with_transient_pools_cleared`]
    /// is safer because it leaves the old generation available during migration.
    pub fn clear_transient_pools(&mut self) {
        let styles = self.styles.clone();
        let style_ids = self.style_ids.clone();
        *self = Self::default();
        self.styles = styles;
        self.style_ids = style_ids;
    }
}

fn skip_escape_sequence_packed_styled_clusters(
    clusters: &[CanvasPackedStyledLineCluster],
    idx: usize,
) -> usize {
    let Some(next) = clusters
        .get(idx + 1)
        .and_then(|cluster| single_ascii_byte(&cluster.text))
    else {
        return idx + 1;
    };

    match next {
        b'(' | b')' | b'*' | b'+' => (idx + 3).min(clusters.len()),
        b'[' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text)
                    .is_some_and(|byte| (0x40..=0x7e).contains(&byte))
                {
                    return j + 1;
                }
                j += 1;
            }
            clusters.len()
        }
        b']' | b'P' | b'_' | b'^' | b'X' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text) == Some(0x07) {
                    return j + 1;
                }
                if single_ascii_byte(&clusters[j].text) == Some(0x1b)
                    && clusters
                        .get(j + 1)
                        .and_then(|cluster| single_ascii_byte(&cluster.text))
                        == Some(b'\\')
                {
                    return (j + 2).min(clusters.len());
                }
                j += 1;
            }
            clusters.len()
        }
        0x30..=0x7e => (idx + 2).min(clusters.len()),
        _ => idx + 1,
    }
}

/// Packed retained-canvas snapshot produced by [`Canvas::pack_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasPackedScreen {
    /// Width in terminal cells.
    pub width: usize,
    /// Height in terminal rows.
    pub height: usize,
    /// Row-major packed cells using IDs from the pools passed to [`Canvas::pack_with`].
    pub cells: Vec<CanvasPackedCell>,
    /// Row-major no-select bitmap copied from the canvas.
    pub no_select: Vec<bool>,
    /// Per-row soft-wrap continuation/content-end metadata.
    pub soft_wrap: Vec<usize>,
    /// One-shot damage region captured from the canvas.
    pub damage_region: Option<DamageRegion>,
}

/// Per-cell change emitted by [`CanvasPackedScreen::diff`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPackedDiffChange {
    /// Column of the changed cell.
    pub x: usize,
    /// Row of the changed cell.
    pub y: usize,
    /// Previous packed cell, or `None` when the next screen grew into this position.
    pub removed: Option<CanvasPackedCell>,
    /// New packed cell, or `None` when the next screen shrank away from this position.
    pub added: Option<CanvasPackedCell>,
}

/// Clip bounds for [`CanvasPackedOutput`] operations.
///
/// Each bound is optional; `None` means unbounded on that side. Bounds are
/// intersected when clips are nested, mirroring CC Ink `Output.clip(...)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanvasPackedOutputClip {
    /// Inclusive left bound.
    pub x1: Option<usize>,
    /// Exclusive right bound.
    pub x2: Option<usize>,
    /// Inclusive top bound.
    pub y1: Option<usize>,
    /// Exclusive bottom bound.
    pub y2: Option<usize>,
}

/// Queued packed output operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasPackedOutputOperation {
    /// Queue a styled text write at a screen-space coordinate.
    Write {
        /// Start column.
        x: usize,
        /// Start row.
        y: usize,
        /// Pre-split logical lines. Each line contains one or more styled runs.
        lines: Vec<Vec<CanvasPackedLineOwnedRun>>,
        /// Optional soft-wrap flags parallel to `lines`.
        soft_wrap: Option<Vec<bool>>,
    },
    /// Push a nested clip rectangle.
    Clip(CanvasPackedOutputClip),
    /// Pop the current clip rectangle.
    Unclip,
    /// Copy a same-coordinate rectangle from another packed screen.
    Blit {
        /// Source screen snapshot.
        src: CanvasPackedScreen,
        /// Left column.
        x: usize,
        /// Top row.
        y: usize,
        /// Width in cells.
        width: usize,
        /// Height in rows.
        height: usize,
    },
    /// Mark a region dirty so stale previous-frame content is cleared by diffing.
    Clear {
        /// Region to mark damaged.
        region: DamageRegion,
        /// Whether this clear came from an absolute-positioned overlay.
        from_absolute: bool,
    },
    /// Mark a region as excluded from selection/search text.
    NoSelect {
        /// Region to mark as no-select.
        region: DamageRegion,
    },
    /// Shift full-width rows inside an inclusive row range.
    Shift {
        /// Top row, inclusive.
        top: usize,
        /// Bottom row, inclusive.
        bottom: usize,
        /// Positive values move rows up; negative values move rows down.
        delta: i32,
    },
}

/// Opt-in packed equivalent of CC Ink's `Output` operation queue.
///
/// The built-in iocraft renderer still writes directly to typed [`Canvas`].
/// Custom retained renderers can use `CanvasPackedOutput` when they want the
/// CC Ink ordering model without making packed screens the framework default:
/// clear operations first expand damage, blits honor active clips and skip rows
/// fully covered by absolute clears, writes use [`CanvasPackedLineCache`], shifts
/// mirror packed `shiftRows(...)`, and no-select metadata is replayed last so it
/// wins over blits and writes.
#[derive(Clone, Debug)]
pub struct CanvasPackedOutput {
    width: usize,
    height: usize,
    screen: CanvasPackedScreen,
    operations: Vec<CanvasPackedOutputOperation>,
    line_cache: CanvasPackedLineCache,
}

impl CanvasPackedScreen {
    /// Creates an empty packed screen using the default pool IDs.
    ///
    /// The returned screen assumes the standard [`CanvasPackedCellPools`]
    /// defaults: space char ID 0, spacer/empty char ID 1, no hyperlink ID 0,
    /// and default style ID 0. It is useful for custom retained renderers that
    /// want to build a packed buffer directly instead of first drawing a
    /// [`Canvas`] and calling [`Canvas::pack_with`].
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![Self::empty_cell(); width.saturating_mul(height)],
            no_select: vec![false; width.saturating_mul(height)],
            soft_wrap: vec![0; height],
            damage_region: None,
        }
    }

    /// Resets this packed screen for reuse, clearing active cells and metadata.
    ///
    /// This mirrors CC Ink's `resetScreen(...)` reuse path for retained double
    /// buffers: vectors may keep their allocation, dimensions are updated,
    /// cells/no-select/soft-wrap metadata are cleared for the active area, and
    /// damage is reset. It does not clear or mutate the external intern pools.
    pub fn reset(&mut self, width: usize, height: usize) {
        let size = width.saturating_mul(height);
        self.width = width;
        self.height = height;
        self.cells.resize(size, Self::empty_cell());
        self.cells.fill(Self::empty_cell());
        self.no_select.resize(size, false);
        self.no_select.fill(false);
        self.soft_wrap.resize(height, 0);
        self.soft_wrap.fill(0);
        self.damage_region = None;
    }

    fn empty_cell() -> CanvasPackedCell {
        CanvasPackedCell::default()
    }

    fn width_tail_cell() -> CanvasPackedCell {
        CanvasPackedCell {
            char_id: 1,
            style_id: 0,
            hyperlink_id: 0,
            width: CanvasPackedCellWidth::WidthTail,
        }
    }

    fn mark_damage_region(&mut self, region: DamageRegion) {
        if region.width == 0 || region.height == 0 {
            return;
        }
        self.damage_region = Some(
            self.damage_region
                .map_or(region, |damage| damage.union(region)),
        );
    }

    /// Returns the row-major index for a cell, if it is in bounds.
    pub fn index(&self, x: usize, y: usize) -> Option<usize> {
        (x < self.width && y < self.height).then_some(y * self.width + x)
    }

    /// Returns a packed cell by coordinates.
    pub fn cell(&self, x: usize, y: usize) -> Option<CanvasPackedCell> {
        self.index(x, y)
            .and_then(|index| self.cells.get(index).copied())
    }

    /// Returns the resolved character at a cell, or `None` when out of bounds.
    ///
    /// This mirrors CC Ink's `charInCellAt(...)`: spacer tails resolve to the
    /// pool's empty string and unwritten cells resolve to a literal space.
    pub fn char_in_cell<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
    ) -> Option<&'a str> {
        self.cell(x, y)
            .map(|cell| pools.character(cell.char_id).unwrap_or(" "))
    }

    /// Returns a resolved borrowed cell view by row-major index.
    pub fn cell_view_at_index<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        index: usize,
    ) -> Option<CanvasPackedCellView<'a>> {
        self.cells
            .get(index)
            .copied()
            .map(|cell| pools.cell_view(cell))
    }

    /// Returns a resolved borrowed cell view by coordinates.
    pub fn cell_view<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
    ) -> Option<CanvasPackedCellView<'a>> {
        let index = self.index(x, y)?;
        self.cell_view_at_index(pools, index)
    }

    /// Returns whether a packed cell is visually empty by pool-default IDs.
    pub fn is_empty_cell(&self, x: usize, y: usize) -> bool {
        self.cell(x, y)
            .map(CanvasPackedCell::is_empty)
            .unwrap_or(true)
    }

    /// Returns whether a cell is marked no-select.
    pub fn is_no_select(&self, x: usize, y: usize) -> bool {
        self.index(x, y)
            .and_then(|index| self.no_select.get(index))
            .copied()
            .unwrap_or(false)
    }

    /// Returns the current packed-screen damage region.
    pub fn damage_region(&self) -> Option<DamageRegion> {
        self.damage_region
    }

    /// Adds a packed-screen damage region without changing cells.
    ///
    /// This is useful for custom output collectors that mirror CC Ink's
    /// `Output.clear(...)` pass: the next buffer may already be blank, while
    /// the diff still needs to scan the old bounds to clear stale terminal
    /// output from the previous frame.
    pub fn mark_damage(&mut self, region: DamageRegion) {
        self.mark_damage_region(region);
    }

    /// Clears one-shot packed-screen damage metadata.
    pub fn clear_damage(&mut self) {
        self.damage_region = None;
    }

    /// Returns the soft-wrap continuation/content-end marker for a row.
    pub fn soft_wrap_continuation(&self, row: usize) -> usize {
        self.soft_wrap.get(row).copied().unwrap_or(0)
    }

    /// Returns the sparse-render-visible cell at row-major `index`, if any.
    ///
    /// This is the packed-screen counterpart to CC Ink's
    /// `visibleCellAtIndex(...)`. It does not allocate or write terminal output;
    /// custom packed renderers can use it to skip spacer/default-space cells
    /// while preserving spaces whose style or hyperlink has visible semantics.
    pub fn visible_cell_at_index(
        &self,
        pools: &CanvasPackedCellPools,
        index: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCell> {
        let cell = self.cells.get(index).copied()?;
        pools
            .cell_visible_for_sparse_render(cell, last_rendered_style_id)
            .then_some(cell)
    }

    /// Returns the sparse-render-visible packed cell by coordinates, if any.
    pub fn visible_cell(
        &self,
        pools: &CanvasPackedCellPools,
        x: usize,
        y: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCell> {
        let index = self.index(x, y)?;
        self.visible_cell_at_index(pools, index, last_rendered_style_id)
    }

    /// Returns the sparse-render-visible resolved cell view at row-major `index`.
    pub fn visible_cell_view_at_index<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        index: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCellView<'a>> {
        self.visible_cell_at_index(pools, index, last_rendered_style_id)
            .map(|cell| pools.cell_view(cell))
    }

    /// Returns the sparse-render-visible resolved cell view by coordinates.
    pub fn visible_cell_view<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCellView<'a>> {
        let index = self.index(x, y)?;
        self.visible_cell_view_at_index(pools, index, last_rendered_style_id)
    }

    /// Writes one packed cell, repairing wide-cell relationships and marking damage.
    ///
    /// This is the Rust-first counterpart to CC Ink's packed `setCellAt(...)`:
    /// overwriting a wide head clears its old tail, overwriting a wide tail
    /// clears the orphaned head, and writing a new wide cell creates the tail in
    /// the following column. The method expects all IDs in `cell` to come from
    /// the caller's compatible [`CanvasPackedCellPools`].
    pub fn set_cell(&mut self, x: usize, y: usize, cell: CanvasPackedCell) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };

        let previous_width = self.cells[index].width;
        if previous_width == CanvasPackedCellWidth::Wide
            && cell.width != CanvasPackedCellWidth::Wide
            && x + 1 < self.width
        {
            let tail_index = index + 1;
            if self.cells[tail_index].width == CanvasPackedCellWidth::WidthTail {
                self.cells[tail_index] = Self::empty_cell();
            }
        }

        let mut damage_min_x = x;
        if previous_width == CanvasPackedCellWidth::WidthTail
            && cell.width != CanvasPackedCellWidth::WidthTail
            && x > 0
        {
            let head_index = index - 1;
            if self.cells[head_index].width == CanvasPackedCellWidth::Wide {
                self.cells[head_index] = Self::empty_cell();
                damage_min_x = x - 1;
            }
        }

        self.cells[index] = cell;
        self.mark_damage_region(DamageRegion {
            x: damage_min_x,
            y,
            width: x.saturating_sub(damage_min_x) + 1,
            height: 1,
        });

        if cell.width == CanvasPackedCellWidth::Wide && x + 1 < self.width {
            let tail_index = index + 1;
            if self.cells[tail_index].width == CanvasPackedCellWidth::Wide && x + 2 < self.width {
                let orphan_tail = index + 2;
                if self.cells[orphan_tail].width == CanvasPackedCellWidth::WidthTail {
                    self.cells[orphan_tail] = Self::empty_cell();
                }
            }
            self.cells[tail_index] = Self::width_tail_cell();
            self.mark_damage_region(DamageRegion {
                x,
                y,
                width: 2,
                height: 1,
            });
        }

        true
    }

    /// Interns and writes one packed cell from typed text/style/link inputs.
    pub fn set_cell_text(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        text: &str,
        style: CanvasResolvedStyle,
        hyperlink: Option<&str>,
        width: CanvasPackedCellWidth,
    ) -> bool {
        let char_text = match width {
            CanvasPackedCellWidth::WidthTail => "",
            CanvasPackedCellWidth::SpacerHead => " ",
            CanvasPackedCellWidth::Normal | CanvasPackedCellWidth::Wide => text,
        };
        let cell = CanvasPackedCell {
            char_id: pools.intern_char(char_text),
            style_id: pools.intern_style(style),
            hyperlink_id: pools.intern_hyperlink(hyperlink),
            width,
        };
        self.set_cell(x, y, cell)
    }

    /// Writes one logical line using cached grapheme widths and explicit style/link IDs.
    ///
    /// This mirrors CC Ink's `writeLineToScreen(...)` hot-loop shape for custom
    /// packed renderers: TABs expand to 8-column stops using default styling,
    /// C0 controls and raw terminal escape sequences are skipped, zero-width
    /// clusters do not mutate cells, and a wide grapheme at the right edge writes
    /// a `SpacerHead` placeholder instead of allowing terminal autowrap. The
    /// return value is the visual end column used by soft-wrap metadata.
    pub fn write_line_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        line: &str,
        style_id: u32,
        hyperlink_id: u32,
    ) -> usize {
        if y >= self.height {
            return x;
        }

        let clusters = cache.clusters(line);
        self.write_cached_clusters_with_ids(pools, clusters, x, y, style_id, hyperlink_id)
    }

    fn write_cached_clusters_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        clusters: &[CanvasPackedLineCluster],
        mut offset_x: usize,
        y: usize,
        style_id: u32,
        hyperlink_id: u32,
    ) -> usize {
        let mut cluster_idx = 0;
        while cluster_idx < clusters.len() {
            let cluster = &clusters[cluster_idx];
            if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
                if code == b'\t' {
                    let spaces_to_next_stop = 8 - (offset_x % 8);
                    for _ in 0..spaces_to_next_stop {
                        if offset_x < self.width {
                            self.set_cell(
                                offset_x,
                                y,
                                CanvasPackedCell {
                                    char_id: 0,
                                    style_id: 0,
                                    hyperlink_id: 0,
                                    width: CanvasPackedCellWidth::Normal,
                                },
                            );
                        }
                        offset_x = offset_x.saturating_add(1);
                    }
                    cluster_idx += 1;
                    continue;
                }

                cluster_idx = if code == 0x1b {
                    skip_escape_sequence_packed_clusters(clusters, cluster_idx)
                } else {
                    cluster_idx + 1
                };
                continue;
            }

            let char_width = cluster.width;
            if char_width == 0 {
                cluster_idx += 1;
                continue;
            }

            let is_wide = char_width >= 2;
            if is_wide && offset_x.saturating_add(2) > self.width {
                if offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                }
                offset_x = offset_x.saturating_add(1);
                cluster_idx += 1;
                continue;
            }

            if offset_x < self.width {
                self.set_cell(
                    offset_x,
                    y,
                    CanvasPackedCell {
                        char_id: pools.intern_char(&cluster.text),
                        style_id,
                        hyperlink_id,
                        width: if is_wide {
                            CanvasPackedCellWidth::Wide
                        } else {
                            CanvasPackedCellWidth::Normal
                        },
                    },
                );
            }
            offset_x = offset_x.saturating_add(if is_wide { 2 } else { 1 });
            cluster_idx += 1;
        }

        offset_x
    }

    fn write_cached_clusters_clipped_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        clusters: &[CanvasPackedLineCluster],
        mut offset_x: usize,
        y: usize,
        style_id: u32,
        hyperlink_id: u32,
        min_x: usize,
        max_x: usize,
    ) -> usize {
        if y >= self.height || min_x >= max_x {
            return offset_x;
        }

        let mut cluster_idx = 0;
        while cluster_idx < clusters.len() {
            let cluster = &clusters[cluster_idx];
            if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
                if code == b'\t' {
                    let spaces_to_next_stop = 8 - (offset_x % 8);
                    for _ in 0..spaces_to_next_stop {
                        if offset_x >= min_x && offset_x < max_x && offset_x < self.width {
                            self.set_cell(
                                offset_x,
                                y,
                                CanvasPackedCell {
                                    char_id: 0,
                                    style_id: 0,
                                    hyperlink_id: 0,
                                    width: CanvasPackedCellWidth::Normal,
                                },
                            );
                        }
                        offset_x = offset_x.saturating_add(1);
                    }
                    cluster_idx += 1;
                    continue;
                }

                cluster_idx = if code == 0x1b {
                    skip_escape_sequence_packed_clusters(clusters, cluster_idx)
                } else {
                    cluster_idx + 1
                };
                continue;
            }

            let char_width = cluster.width;
            if char_width == 0 {
                cluster_idx += 1;
                continue;
            }

            let next_x = offset_x.saturating_add(char_width.max(1));
            if next_x <= min_x {
                offset_x = next_x;
                cluster_idx += 1;
                continue;
            }
            if offset_x >= max_x {
                break;
            }
            if offset_x < min_x {
                // Avoid rendering a partial grapheme clipped at the left edge.
                offset_x = next_x;
                cluster_idx += 1;
                continue;
            }

            let is_wide = char_width >= 2;
            if next_x > max_x {
                if is_wide && max_x >= self.width && offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                    offset_x = offset_x.saturating_add(1);
                }
                break;
            }

            if is_wide && offset_x.saturating_add(2) > self.width {
                if offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                }
                offset_x = offset_x.saturating_add(1);
                cluster_idx += 1;
                continue;
            }

            if offset_x < self.width {
                self.set_cell(
                    offset_x,
                    y,
                    CanvasPackedCell {
                        char_id: pools.intern_char(&cluster.text),
                        style_id,
                        hyperlink_id,
                        width: if is_wide {
                            CanvasPackedCellWidth::Wide
                        } else {
                            CanvasPackedCellWidth::Normal
                        },
                    },
                );
            }
            offset_x = next_x;
            cluster_idx += 1;
        }

        offset_x
    }

    /// Writes adjacent pre-interned styled runs with a shared line cache.
    ///
    /// This mirrors the CC Ink `styledCharsWithGraphemeClustering(...)` +
    /// `writeLineToScreen(...)` hot path after ANSI tokenization: style and
    /// hyperlink IDs are resolved once per run, grapheme widths are reused by
    /// [`CanvasPackedLineCache`], and the full line is reordered for terminals
    /// that need software bidi before the LTR cell-placement loop. The returned
    /// column is the visual end after all runs, including TAB expansion and
    /// skipped controls.
    pub fn write_line_runs_with_ids<'a, I>(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        runs: I,
    ) -> usize
    where
        I: IntoIterator<Item = CanvasPackedLineRunIds<'a>>,
    {
        self.write_line_runs_with_ids_bidi_mode(
            pools,
            cache,
            x,
            y,
            runs,
            crate::bidi::needs_software_bidi(),
        )
    }

    pub(super) fn write_line_runs_with_ids_bidi_mode<'a, I>(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        runs: I,
        bidi_enabled: bool,
    ) -> usize
    where
        I: IntoIterator<Item = CanvasPackedLineRunIds<'a>>,
    {
        if y >= self.height {
            return x;
        }

        let runs = runs.into_iter().collect::<Vec<_>>();
        if !bidi_enabled {
            let mut offset_x = x;
            for run in runs {
                let clusters = cache.clusters(run.text);
                offset_x = self.write_cached_clusters_with_ids(
                    pools,
                    clusters,
                    offset_x,
                    y,
                    run.style_id,
                    run.hyperlink_id,
                );
            }
            return offset_x;
        }

        let mut graphemes = Vec::new();
        for run in runs {
            for cluster in cache.clusters(run.text) {
                graphemes.push(crate::bidi::BidiGrapheme {
                    text: cluster.text.clone(),
                    metadata: (cluster.width, run.style_id, run.hyperlink_id),
                });
            }
        }
        let reordered = crate::bidi::reorder_bidi_graphemes(graphemes, true)
            .into_iter()
            .map(|item| CanvasPackedStyledLineCluster {
                text: item.text,
                width: item.metadata.0,
                style_id: item.metadata.1,
                hyperlink_id: item.metadata.2,
            })
            .collect::<Vec<_>>();
        self.write_styled_clusters_with_ids(pools, &reordered, x, y)
    }

    fn write_styled_clusters_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        clusters: &[CanvasPackedStyledLineCluster],
        mut offset_x: usize,
        y: usize,
    ) -> usize {
        let mut cluster_idx = 0;
        while cluster_idx < clusters.len() {
            let cluster = &clusters[cluster_idx];
            if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
                if code == b'\t' {
                    let spaces_to_next_stop = 8 - (offset_x % 8);
                    for _ in 0..spaces_to_next_stop {
                        if offset_x < self.width {
                            self.set_cell(
                                offset_x,
                                y,
                                CanvasPackedCell {
                                    char_id: 0,
                                    style_id: 0,
                                    hyperlink_id: 0,
                                    width: CanvasPackedCellWidth::Normal,
                                },
                            );
                        }
                        offset_x = offset_x.saturating_add(1);
                    }
                    cluster_idx += 1;
                    continue;
                }

                cluster_idx = if code == 0x1b {
                    skip_escape_sequence_packed_styled_clusters(clusters, cluster_idx)
                } else {
                    cluster_idx + 1
                };
                continue;
            }

            let char_width = cluster.width;
            if char_width == 0 {
                cluster_idx += 1;
                continue;
            }

            let is_wide = char_width >= 2;
            if is_wide && offset_x.saturating_add(2) > self.width {
                if offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                }
                offset_x = offset_x.saturating_add(1);
                cluster_idx += 1;
                continue;
            }

            if offset_x < self.width {
                self.set_cell(
                    offset_x,
                    y,
                    CanvasPackedCell {
                        char_id: pools.intern_char(&cluster.text),
                        style_id: cluster.style_id,
                        hyperlink_id: cluster.hyperlink_id,
                        width: if is_wide {
                            CanvasPackedCellWidth::Wide
                        } else {
                            CanvasPackedCellWidth::Normal
                        },
                    },
                );
            }
            offset_x = offset_x.saturating_add(if is_wide { 2 } else { 1 });
            cluster_idx += 1;
        }

        offset_x
    }

    /// Interns typed style/link metadata once per run and writes adjacent runs.
    pub fn write_line_runs_with_cache<'a, I>(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        runs: I,
    ) -> usize
    where
        I: IntoIterator<Item = CanvasPackedLineRun<'a>>,
    {
        let runs = runs
            .into_iter()
            .map(|run| CanvasPackedLineRunIds {
                text: run.text,
                style_id: pools.intern_style(run.style),
                hyperlink_id: pools.intern_hyperlink(run.hyperlink),
            })
            .collect::<Vec<_>>();
        self.write_line_runs_with_ids(pools, cache, x, y, runs)
    }

    /// Interns style/link inputs and writes one logical line with a line cache.
    pub fn write_line_with_cache(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        line: &str,
        style: CanvasResolvedStyle,
        hyperlink: Option<&str>,
    ) -> usize {
        let style_id = pools.intern_style(style);
        let hyperlink_id = pools.intern_hyperlink(hyperlink);
        self.write_line_with_ids(pools, cache, x, y, line, style_id, hyperlink_id)
    }

    /// Re-interns char/hyperlink IDs from one transient pool generation into another.
    ///
    /// This is the opt-in Rust counterpart to CC Ink's `migrateScreenPools(...)`.
    /// It preserves packed cell contents, style IDs, width markers, damage,
    /// no-select metadata, and soft-wrap metadata while remapping only the
    /// transient character and hyperlink IDs. `new_pools` should normally be
    /// created with [`CanvasPackedCellPools::fork_with_transient_pools_cleared`]
    /// so style IDs remain valid across generations.
    pub fn migrate_transient_pools(
        &mut self,
        old_pools: &CanvasPackedCellPools,
        new_pools: &mut CanvasPackedCellPools,
    ) -> bool {
        let mut changed = false;
        for cell in &mut self.cells {
            let char_text = old_pools.character(cell.char_id).unwrap_or(" ");
            let new_char_id = new_pools.intern_char(char_text);
            if new_char_id != cell.char_id {
                cell.char_id = new_char_id;
                changed = true;
            }

            let hyperlink = old_pools.hyperlink(cell.hyperlink_id);
            let new_hyperlink_id = new_pools.intern_hyperlink(hyperlink);
            if new_hyperlink_id != cell.hyperlink_id {
                cell.hyperlink_id = new_hyperlink_id;
                changed = true;
            }
        }
        changed
    }

    /// Replaces the interned style ID of a non-spacer cell and marks it damaged.
    ///
    /// This is the packed-screen counterpart to CC Ink's `setCellStyleId(...)`:
    /// the character, width marker, and hyperlink ID are preserved, wide tails
    /// and spacer heads are skipped because styling the wide head visually covers
    /// the full grapheme, and the cell is added to the damage region so packed
    /// diffs see the post-render style change.
    pub fn set_cell_style_id(&mut self, x: usize, y: usize, style_id: u32) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };
        match self.cells[index].width {
            CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead => return false,
            CanvasPackedCellWidth::Normal | CanvasPackedCellWidth::Wide => {}
        }
        self.cells[index].style_id = style_id;
        self.mark_damage_region(DamageRegion {
            x,
            y,
            width: 1,
            height: 1,
        });
        true
    }

    /// Interns and applies a resolved style to a non-spacer packed cell.
    pub fn set_cell_style(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        style: CanvasResolvedStyle,
    ) -> bool {
        let style_id = pools.intern_style(style);
        self.set_cell_style_id(x, y, style_id)
    }

    /// Applies a post-render style overlay to one packed cell and marks it damaged.
    ///
    /// This is the packed-screen counterpart to [`Canvas::set_overlay`]: the
    /// overlay is composed into the cell's interned resolved style, wide tails
    /// are redirected to their leading wide cell, and spacer heads are ignored
    /// because they have no independently rendered glyph. It is intended for
    /// custom packed renderers that apply selection/search/debug overlays after
    /// drawing without unpacking back into [`Canvas`].
    pub fn apply_style_overlay(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        overlay: StyleOverlay,
    ) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };
        let target_x = match self.cells[index].width {
            CanvasPackedCellWidth::WidthTail if x > 0 => {
                let previous = index - 1;
                if self.cells[previous].width == CanvasPackedCellWidth::Wide {
                    x - 1
                } else {
                    return false;
                }
            }
            CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead => return false,
            CanvasPackedCellWidth::Normal | CanvasPackedCellWidth::Wide => x,
        };
        let Some(target_index) = self.index(target_x, y) else {
            return false;
        };
        let style_id = pools.style_id_with_overlay(self.cells[target_index].style_id, overlay);
        self.set_cell_style_id(target_x, y, style_id)
    }

    /// Applies a post-render style overlay to a rectangular packed-screen region.
    pub fn apply_style_overlay_region(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        overlay: StyleOverlay,
    ) -> bool {
        let start_x = x.min(self.width);
        let start_y = y.min(self.height);
        let max_x = x.saturating_add(width).min(self.width);
        let max_y = y.saturating_add(height).min(self.height);
        let mut changed = false;
        for row in start_y..max_y {
            for col in start_x..max_x {
                changed |= self.apply_style_overlay(pools, col, row, overlay);
            }
        }
        changed
    }

    fn selection_char_class(value: &str) -> u8 {
        if value.is_empty() || value == " " {
            return 0;
        }
        if value.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '/' | '.' | '-' | '+' | '~' | '\\')
        }) {
            1
        } else {
            2
        }
    }

    fn cell_selection_text<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
    ) -> Option<&'a str> {
        self.cell(x, y)
            .and_then(|cell| pools.character(cell.char_id))
            .or(Some(" "))
    }

    /// Returns inclusive same-class word bounds for a packed-screen cell.
    ///
    /// This is the packed counterpart to CC Ink's double-click `wordBoundsAt`
    /// behavior used by `selectWordAt(...)`: if the target is a wide tail, the
    /// head cell is selected; no-select cells stop expansion; and word classes
    /// match terminal-like path/token classes.
    pub fn word_bounds_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<(usize, usize)> {
        if row >= self.height || self.width == 0 {
            return None;
        }

        let mut c = col;
        if c > 0
            && self
                .cell(c, row)
                .is_some_and(|cell| cell.width == CanvasPackedCellWidth::WidthTail)
        {
            c -= 1;
        }
        if c >= self.width || self.is_no_select(c, row) {
            return None;
        }
        let cls = Self::selection_char_class(self.cell_selection_text(pools, c, row)?);

        let mut lo = c;
        while lo > 0 {
            let prev = lo - 1;
            if self.is_no_select(prev, row) {
                break;
            }
            let Some(prev_cell) = self.cell(prev, row) else {
                break;
            };
            if prev_cell.width == CanvasPackedCellWidth::WidthTail {
                if prev == 0 || self.is_no_select(prev - 1, row) {
                    break;
                }
                let Some(head_text) = self.cell_selection_text(pools, prev - 1, row) else {
                    break;
                };
                if Self::selection_char_class(head_text) != cls {
                    break;
                }
                lo = prev - 1;
                continue;
            }
            let Some(prev_text) = self.cell_selection_text(pools, prev, row) else {
                break;
            };
            if Self::selection_char_class(prev_text) != cls {
                break;
            }
            lo = prev;
        }

        let mut hi = c;
        while hi + 1 < self.width {
            let next = hi + 1;
            if self.is_no_select(next, row) {
                break;
            }
            let Some(next_cell) = self.cell(next, row) else {
                break;
            };
            if next_cell.width == CanvasPackedCellWidth::WidthTail {
                hi = next;
                continue;
            }
            let Some(next_text) = self.cell_selection_text(pools, next, row) else {
                break;
            };
            if Self::selection_char_class(next_text) != cls {
                break;
            }
            hi = next;
        }

        Some((lo, hi))
    }

    /// Returns the linear selection range for a double-click word selection.
    pub fn selection_range_for_word_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<SelectionRange> {
        let (lo, hi) = self.word_bounds_at(pools, col, row)?;
        Some(SelectionRange::new(
            SelectionPoint { col: lo, row },
            SelectionPoint { col: hi, row },
        ))
    }

    /// Returns the linear selection range for a triple-click row selection.
    pub fn selection_range_for_line(&self, row: usize) -> Option<SelectionRange> {
        if row >= self.height || self.width == 0 {
            return None;
        }
        Some(SelectionRange::new(
            SelectionPoint { col: 0, row },
            SelectionPoint {
                col: self.width - 1,
                row,
            },
        ))
    }

    pub(super) fn extract_selected_row(
        &self,
        pools: &CanvasPackedCellPools,
        row: usize,
        col_start: usize,
        col_end: usize,
    ) -> String {
        let content_end = self.soft_wrap_continuation(row + 1);
        let last_col = if content_end > 0 {
            col_end.min(content_end.saturating_sub(1))
        } else {
            col_end
        };
        let mut line = String::new();
        for col in col_start..=last_col {
            if self.is_no_select(col, row) {
                continue;
            }
            let Some(cell) = self.cell(col, row) else {
                continue;
            };
            if matches!(
                cell.width,
                CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
            ) {
                continue;
            }
            line.push_str(self.char_in_cell(pools, col, row).unwrap_or(" "));
        }
        if content_end > 0 {
            line
        } else {
            line.trim_end().to_string()
        }
    }

    /// Extracts text from a linear packed-screen selection range.
    ///
    /// This mirrors [`Canvas::selected_text`] and CC Ink's
    /// `getSelectedText(...)`: no-select cells are skipped, soft-wrap
    /// continuation rows are joined without a newline, and wide/spacer cells do
    /// not duplicate graphemes. It is useful for custom packed renderers that
    /// want selection/copy behavior without converting back to [`Canvas`].
    pub fn selected_text(&self, pools: &CanvasPackedCellPools, range: SelectionRange) -> String {
        let (start, end) = range.normalized();
        if self.width == 0 || self.height == 0 || start.row >= self.height {
            return String::new();
        }

        let mut lines = Vec::<String>::new();
        let last_row = end.row.min(self.height - 1);
        for row in start.row..=last_row {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(self.width - 1)
            } else {
                self.width - 1
            };
            let line = if col_start >= self.width || col_start > col_end {
                String::new()
            } else {
                self.extract_selected_row(pools, row, col_start, col_end)
            };

            if self.soft_wrap_continuation(row) > 0 && !lines.is_empty() {
                lines.last_mut().unwrap().push_str(&line);
            } else {
                lines.push(line);
            }
        }

        lines.join("\n")
    }

    /// Applies a style overlay to every selectable packed cell in a linear range.
    ///
    /// This is the packed counterpart to [`Canvas::apply_selection_overlay`]: it
    /// skips no-select/spacer cells, composes the overlay into interned style IDs
    /// after rendering, and marks changed cells damaged so an otherwise-identical
    /// packed diff repaints the selection/search highlight.
    pub fn apply_selection_overlay(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        range: SelectionRange,
        overlay: StyleOverlay,
    ) -> bool {
        let (start, end) = range.normalized();
        if self.width == 0 || self.height == 0 || start.row >= self.height {
            return false;
        }

        let mut changed = false;
        let last_row = end.row.min(self.height - 1);
        for row in start.row..=last_row {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(self.width - 1)
            } else {
                self.width - 1
            };
            if col_start >= self.width || col_start > col_end {
                continue;
            }
            for col in col_start..=col_end {
                if self.is_no_select(col, row) {
                    continue;
                }
                let Some(cell) = self.cell(col, row) else {
                    continue;
                };
                if matches!(
                    cell.width,
                    CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
                ) {
                    continue;
                }
                changed |= self.apply_style_overlay(pools, col, row, overlay);
            }
        }
        changed
    }

    fn searchable_row_text_in_cols(
        &self,
        pools: &CanvasPackedCellPools,
        row: usize,
        start_col: usize,
        end_col: usize,
    ) -> (String, Vec<usize>, Vec<usize>) {
        let mut text = String::new();
        let mut col_of_cell = Vec::new();
        let mut byte_to_cell = Vec::new();
        let end_col = end_col.min(self.width);
        for col in start_col.min(end_col)..end_col {
            if self.is_no_select(col, row) {
                continue;
            }
            let Some(cell) = self.cell(col, row) else {
                continue;
            };
            if matches!(
                cell.width,
                CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
            ) {
                continue;
            }

            let lower = self
                .char_in_cell(pools, col, row)
                .unwrap_or(" ")
                .to_lowercase();
            let cell_idx = col_of_cell.len();
            byte_to_cell.extend(std::iter::repeat_n(cell_idx, lower.len()));
            text.push_str(&lower);
            col_of_cell.push(col);
        }
        (text, col_of_cell, byte_to_cell)
    }

    fn scan_text_positions_absolute(
        &self,
        pools: &CanvasPackedCellPools,
        query: &str,
        start_x: usize,
        start_y: usize,
        max_x: usize,
        max_y: usize,
        relative_to_region: bool,
    ) -> Vec<TextMatchPosition> {
        let query = query.to_lowercase();
        if query.is_empty() || self.width == 0 || self.height == 0 {
            return Vec::new();
        }

        let mut positions = Vec::new();
        for row in start_y.min(max_y)..max_y.min(self.height) {
            let (text, col_of_cell, byte_to_cell) =
                self.searchable_row_text_in_cols(pools, row, start_x, max_x);
            let mut search_from = 0;
            while search_from <= text.len() {
                let Some(relative_pos) = text[search_from..].find(&query) else {
                    break;
                };
                let pos = search_from + relative_pos;
                let end_byte = pos + query.len() - 1;
                let (Some(&start_cell), Some(&end_cell)) =
                    (byte_to_cell.get(pos), byte_to_cell.get(end_byte))
                else {
                    break;
                };
                let (Some(&start_col), Some(&end_col)) =
                    (col_of_cell.get(start_cell), col_of_cell.get(end_cell))
                else {
                    break;
                };
                let end_width = self
                    .cell(end_col, row)
                    .map(|cell| usize::from(cell.width == CanvasPackedCellWidth::Wide) + 1)
                    .unwrap_or(1);
                let absolute_end = end_col.saturating_add(end_width).min(max_x);
                let (out_row, out_col) = if relative_to_region {
                    (
                        row.saturating_sub(start_y),
                        start_col.saturating_sub(start_x),
                    )
                } else {
                    (row, start_col)
                };
                positions.push(TextMatchPosition {
                    row: out_row,
                    col: out_col,
                    len: absolute_end.saturating_sub(start_col),
                });
                search_from = pos + query.len();
            }
        }
        positions
    }

    /// Scans the packed screen for non-overlapping case-insensitive matches.
    ///
    /// This is the packed counterpart to [`Canvas::scan_text_positions`] and
    /// CC Ink's screen-space search: it searches visible rendered cells, skips
    /// no-select/spacer cells, and reports spans in terminal cells.
    pub fn scan_text_positions(
        &self,
        pools: &CanvasPackedCellPools,
        query: &str,
    ) -> Vec<TextMatchPosition> {
        self.scan_text_positions_absolute(pools, query, 0, 0, self.width, self.height, false)
    }

    /// Scans a packed-screen rectangular region and returns positions relative to it.
    pub fn scan_text_positions_region(
        &self,
        pools: &CanvasPackedCellPools,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        query: &str,
    ) -> Vec<TextMatchPosition> {
        if width == 0 || height == 0 || x >= self.width || y >= self.height {
            return Vec::new();
        }
        self.scan_text_positions_absolute(
            pools,
            query,
            x,
            y,
            x.saturating_add(width).min(self.width),
            y.saturating_add(height).min(self.height),
            true,
        )
    }

    fn apply_overlay_to_match(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        position: TextMatchPosition,
        overlay: StyleOverlay,
    ) -> bool {
        if position.row >= self.height || position.len == 0 {
            return false;
        }
        let end = position.col.saturating_add(position.len).min(self.width);
        let mut changed = false;
        for col in position.col..end {
            if self.is_no_select(col, position.row) {
                continue;
            }
            let Some(cell) = self.cell(col, position.row) else {
                continue;
            };
            if matches!(
                cell.width,
                CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
            ) {
                continue;
            }
            changed |= self.apply_style_overlay(pools, col, position.row, overlay);
        }
        changed
    }

    /// Applies a search-highlight overlay to all visible packed-screen matches.
    ///
    /// Returns `true` if at least one cell was highlighted. Matches are
    /// non-overlapping and case-insensitive, and no-select cells are not search
    /// targets, matching CC Ink's `applySearchHighlight(...)`.
    pub fn apply_search_highlight(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        query: &str,
        overlay: StyleOverlay,
    ) -> bool {
        let positions = self.scan_text_positions(pools, query);
        let mut applied = false;
        for position in positions {
            applied |= self.apply_overlay_to_match(pools, position, overlay);
        }
        applied
    }

    /// Applies an overlay to one pre-scanned packed-screen match plus row offset.
    pub fn apply_positioned_highlight(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        positions: &[TextMatchPosition],
        row_offset: isize,
        current_idx: usize,
        overlay: StyleOverlay,
    ) -> bool {
        let Some(position) = positions.get(current_idx).copied() else {
            return false;
        };
        let row = position.row as isize + row_offset;
        if row < 0 {
            return false;
        }
        self.apply_overlay_to_match(
            pools,
            TextMatchPosition {
                row: row as usize,
                ..position
            },
            overlay,
        )
    }

    fn osc8_hyperlink_at<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<&'a str> {
        let cell = self.cell(col, row)?;
        if let Some(href) = pools.hyperlink(cell.hyperlink_id) {
            return Some(href);
        }
        if col > 0 && cell.width == CanvasPackedCellWidth::WidthTail {
            return self
                .cell(col - 1, row)
                .and_then(|head| pools.hyperlink(head.hyperlink_id));
        }
        None
    }

    fn is_plain_url_char(value: &str) -> bool {
        if value.len() != 1 {
            return false;
        }
        let byte = value.as_bytes()[0];
        (0x21..=0x7e).contains(&byte) && !matches!(byte, b'<' | b'>' | b'"' | b'\'' | b'`' | b' ')
    }

    fn plain_url_char_at<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<&'a str> {
        let cell = self.cell(col, row)?;
        if cell.width != CanvasPackedCellWidth::Normal {
            return None;
        }
        let value = pools.character(cell.char_id)?;
        Self::is_plain_url_char(value).then_some(value)
    }

    /// Finds a plain-text URL at a packed-screen cell, respecting no-select boundaries.
    ///
    /// This mirrors CC Ink's fullscreen fallback used when mouse tracking
    /// intercepts native terminal URL detection. OSC 8 links are handled by
    /// [`CanvasPackedScreen::hyperlink_at`]; this method scans ASCII URL-like
    /// runs for `http://`, `https://`, or `file://` schemes and strips trailing
    /// sentence punctuation.
    pub fn plain_text_url_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<String> {
        if row >= self.height || self.width == 0 {
            return None;
        }
        let mut c = col;
        if c > 0
            && self
                .cell(c, row)
                .is_some_and(|cell| cell.width == CanvasPackedCellWidth::WidthTail)
        {
            c -= 1;
        }
        if c >= self.width
            || self.is_no_select(c, row)
            || self.plain_url_char_at(pools, c, row).is_none()
        {
            return None;
        }

        let mut lo = c;
        while lo > 0 {
            let prev = lo - 1;
            if self.is_no_select(prev, row) || self.plain_url_char_at(pools, prev, row).is_none() {
                break;
            }
            lo = prev;
        }

        let mut hi = c;
        while hi + 1 < self.width {
            let next = hi + 1;
            if self.is_no_select(next, row) || self.plain_url_char_at(pools, next, row).is_none() {
                break;
            }
            hi = next;
        }

        let mut token = String::new();
        for col in lo..=hi {
            token.push_str(self.plain_url_char_at(pools, col, row)?);
        }
        let click_idx = c - lo;

        let mut url_start = None;
        let mut url_end = token.len();
        let mut search_from = 0;
        while search_from < token.len() {
            let rest = &token[search_from..];
            let next_scheme = ["http://", "https://", "file://"]
                .into_iter()
                .filter_map(|scheme| rest.find(scheme).map(|idx| search_from + idx))
                .min();
            let Some(idx) = next_scheme else {
                break;
            };
            if idx > click_idx {
                url_end = idx;
                break;
            }
            url_start = Some(idx);
            search_from = idx + 1;
        }

        let url_start = url_start?;
        let mut url = token[url_start..url_end].to_string();
        while let Some(last) = url.chars().last() {
            if ".,;:!?".contains(last) {
                url.pop();
                continue;
            }
            let opener = match last {
                ')' => Some('('),
                ']' => Some('['),
                '}' => Some('{'),
                _ => None,
            };
            let Some(opener) = opener else {
                break;
            };
            let opens = url.chars().filter(|ch| *ch == opener).count();
            let closes = url.chars().filter(|ch| *ch == last).count();
            if closes > opens {
                url.pop();
            } else {
                break;
            }
        }

        if url.is_empty() || click_idx >= url_start + url.len() {
            return None;
        }
        Some(url)
    }

    /// Returns the hyperlink target at a packed-screen cell.
    ///
    /// OSC 8 hyperlinks are preferred, including when the queried cell is the
    /// tail of a wide linked grapheme. If no OSC 8 hyperlink is present, this
    /// falls back to [`CanvasPackedScreen::plain_text_url_at`], matching CC
    /// Ink's fullscreen click handling while staying a pure buffer lookup.
    pub fn hyperlink_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<String> {
        self.osc8_hyperlink_at(pools, col, row)
            .map(str::to_string)
            .or_else(|| self.plain_text_url_at(pools, col, row))
    }

    /// Returns a clone of `next` with repaint/debug overlay applied to changed packed cells.
    ///
    /// This mirrors [`Canvas::debug_repaint_overlay`] for opt-in packed screens:
    /// packed diffs, previous damage, and current damage are highlighted by
    /// composing `overlay` into style IDs in the returned clone. Overlayed cells
    /// are marked damaged so a custom packed terminal diff can repaint the
    /// visualization without requiring a full unpacked canvas round-trip.
    pub fn debug_repaint_overlay(
        previous: Option<&Self>,
        next: &Self,
        pools: &mut CanvasPackedCellPools,
        overlay: StyleOverlay,
    ) -> Self {
        let mut screen = next.clone();

        match previous {
            Some(previous) => {
                previous.diff_each(next, |change| {
                    screen.apply_style_overlay(pools, change.x, change.y, overlay);
                    false
                });
                if let Some(region) = previous.damage_region() {
                    screen.apply_style_overlay_region(
                        pools,
                        region.x,
                        region.y,
                        region.width,
                        region.height,
                        overlay,
                    );
                }
            }
            None => {
                let empty = CanvasPackedScreen::new(0, 0);
                empty.diff_each(next, |change| {
                    screen.apply_style_overlay(pools, change.x, change.y, overlay);
                    false
                });
            }
        }

        if let Some(region) = next.damage_region() {
            screen.apply_style_overlay_region(
                pools,
                region.x,
                region.y,
                region.width,
                region.height,
                overlay,
            );
        }

        screen
    }

    /// Marks a rectangular packed-screen region as no-select without terminal damage.
    ///
    /// Matching CC Ink's `markNoSelectRegion(...)`, this metadata affects
    /// selection/copy/highlight consumers only; it should not wake terminal
    /// writers by itself.
    pub fn mark_no_select_region(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> bool {
        let start_x = x.min(self.width);
        let start_y = y.min(self.height);
        let max_x = x.saturating_add(width).min(self.width);
        let max_y = y.saturating_add(height).min(self.height);
        if start_x >= max_x || start_y >= max_y {
            return false;
        }
        for row in start_y..max_y {
            let row_start = row * self.width;
            self.no_select[(row_start + start_x)..(row_start + max_x)].fill(true);
        }
        true
    }

    /// Clears a rectangular region of packed cells and returns the damaged rectangle.
    ///
    /// This mirrors CC Ink's packed `clearRegion(...)` boundary behavior for
    /// wide cells: clearing a wide tail also clears its head, and clearing a
    /// wide head also clears the tail just outside the requested rectangle. The
    /// method is opt-in snapshot manipulation; it does not write terminal output.
    pub fn clear_region(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Option<DamageRegion> {
        let start_x = x.min(self.width);
        let start_y = y.min(self.height);
        let max_x = x.saturating_add(width).min(self.width);
        let max_y = y.saturating_add(height).min(self.height);
        if start_x >= max_x || start_y >= max_y {
            return None;
        }

        let mut damage_min_x = start_x;
        let mut damage_max_x = max_x;
        for row in start_y..max_y {
            let row_start = row * self.width;
            if start_x > 0 {
                let start_index = row_start + start_x;
                let previous_index = start_index - 1;
                if self.cells[start_index].width == CanvasPackedCellWidth::WidthTail
                    && self.cells[previous_index].width == CanvasPackedCellWidth::Wide
                {
                    self.cells[previous_index] = Self::empty_cell();
                    if let Some(marked) = self.no_select.get_mut(previous_index) {
                        *marked = false;
                    }
                    damage_min_x = damage_min_x.min(start_x - 1);
                }
            }

            if max_x < self.width {
                let last_index = row_start + max_x - 1;
                let next_index = last_index + 1;
                if self.cells[last_index].width == CanvasPackedCellWidth::Wide
                    && self.cells[next_index].width == CanvasPackedCellWidth::WidthTail
                {
                    self.cells[next_index] = Self::empty_cell();
                    if let Some(marked) = self.no_select.get_mut(next_index) {
                        *marked = false;
                    }
                    damage_max_x = damage_max_x.max(max_x + 1);
                }
            }

            for index in (row_start + start_x)..(row_start + max_x) {
                self.cells[index] = Self::empty_cell();
                if let Some(marked) = self.no_select.get_mut(index) {
                    *marked = false;
                }
            }
            if start_x == 0 && max_x == self.width {
                if let Some(soft_wrap) = self.soft_wrap.get_mut(row) {
                    *soft_wrap = 0;
                }
            }
        }

        let region = DamageRegion {
            x: damage_min_x,
            y: start_y,
            width: damage_max_x.saturating_sub(damage_min_x),
            height: max_y.saturating_sub(start_y),
        };
        self.mark_damage_region(region);
        Some(region)
    }

    /// Copies a rectangular same-coordinate region from another packed screen.
    ///
    /// The two screens must have been produced with compatible
    /// [`CanvasPackedCellPools`] so interned IDs have the same meaning. Like CC
    /// Ink's packed `blitRegion(...)`, this copies no-select metadata and the
    /// affected rows' soft-wrap provenance, marks the copied rectangle damaged,
    /// and repairs a wide-character tail just outside the right edge when the
    /// copied region ends on a wide head.
    pub fn blit_region_from(
        &mut self,
        src: &Self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Option<DamageRegion> {
        let start_x = x.min(self.width).min(src.width);
        let start_y = y.min(self.height).min(src.height);
        let max_x = x.saturating_add(width).min(self.width).min(src.width);
        let max_y = y.saturating_add(height).min(self.height).min(src.height);
        if start_x >= max_x || start_y >= max_y {
            return None;
        }

        for row in start_y..max_y {
            let src_start = row * src.width + start_x;
            let src_end = row * src.width + max_x;
            let dst_start = row * self.width + start_x;
            let dst_end = row * self.width + max_x;
            self.cells[dst_start..dst_end].copy_from_slice(&src.cells[src_start..src_end]);
            self.no_select[dst_start..dst_end].copy_from_slice(&src.no_select[src_start..src_end]);
            if let (Some(dst_soft_wrap), Some(src_soft_wrap)) =
                (self.soft_wrap.get_mut(row), src.soft_wrap.get(row))
            {
                *dst_soft_wrap = *src_soft_wrap;
            }
        }

        let mut damage_width = max_x.saturating_sub(start_x);
        if max_x < self.width && max_x <= src.width {
            let mut wrote_tail = false;
            for row in start_y..max_y {
                let src_last = row * src.width + max_x - 1;
                if src.cells[src_last].width == CanvasPackedCellWidth::Wide {
                    let dst_tail = row * self.width + max_x;
                    self.cells[dst_tail] = Self::width_tail_cell();
                    if let Some(marked) = self.no_select.get_mut(dst_tail) {
                        *marked = false;
                    }
                    wrote_tail = true;
                }
            }
            if wrote_tail {
                damage_width = damage_width.saturating_add(1);
            }
        }

        let region = DamageRegion {
            x: start_x,
            y: start_y,
            width: damage_width,
            height: max_y.saturating_sub(start_y),
        };
        self.mark_damage_region(region);
        Some(region)
    }

    /// Copies a rectangular region while skipping rows covered by absolute clears.
    ///
    /// This is the packed-screen counterpart to CC Ink `output.ts`'s
    /// absolute-clear blit guard and [`Canvas::blit_region_from_excluding_clears`].
    /// A retained sibling blit should not restore rows where a removed absolute
    /// overlay already queued a clear; only rows whose whole blit segment is
    /// covered by a clear are skipped, matching the official
    /// `startX >= clear.x && maxX <= clear.x + clear.width` rule.
    ///
    /// The clear itself is not applied here. Returned regions are the copied
    /// spans that were marked damaged by the underlying blits.
    pub fn blit_region_from_excluding_clears(
        &mut self,
        src: &Self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        excluded_clears: &[DamageRegion],
    ) -> Vec<DamageRegion> {
        if excluded_clears.is_empty() {
            return self
                .blit_region_from(src, x, y, width, height)
                .into_iter()
                .collect();
        }
        if width == 0 || height == 0 || x >= self.width || x >= src.width {
            return Vec::new();
        }

        let start_x = x.min(self.width).min(src.width);
        let start_y = y.min(self.height).min(src.height);
        let max_x = x.saturating_add(width).min(self.width).min(src.width);
        let max_y = y.saturating_add(height).min(self.height).min(src.height);
        if start_x >= max_x || start_y >= max_y {
            return Vec::new();
        }

        let row_is_excluded = |row: usize| {
            excluded_clears.iter().any(|clear| {
                clear.height > 0
                    && row >= clear.y
                    && row < clear.y.saturating_add(clear.height)
                    && start_x >= clear.x
                    && max_x <= clear.x.saturating_add(clear.width)
            })
        };

        let mut regions = Vec::new();
        let mut span_start = start_y;
        for row in start_y..=max_y {
            if row == max_y || row_is_excluded(row) {
                if row > span_start {
                    if let Some(region) = self.blit_region_from(
                        src,
                        start_x,
                        span_start,
                        max_x - start_x,
                        row - span_start,
                    ) {
                        regions.push(region);
                    }
                }
                span_start = row.saturating_add(1);
            }
        }
        regions
    }

    /// Shifts full-width rows inside an inclusive row range.
    ///
    /// Positive `delta` shifts rows up, negative `delta` shifts rows down, and
    /// vacated rows are cleared. Cells, no-select metadata, and soft-wrap row
    /// markers move together. Matching CC Ink's packed `shiftRows(...)`, this
    /// does **not** mark damage; callers pair it with explicit edge repaint or
    /// terminal scroll-hint planning.
    pub fn shift_rows(&mut self, top: usize, bottom: usize, delta: i32) -> bool {
        if delta == 0 || top > bottom || bottom >= self.height || self.width == 0 {
            return false;
        }
        let row_count = bottom - top + 1;
        let abs_delta = delta.unsigned_abs() as usize;
        if abs_delta >= row_count {
            for row in top..=bottom {
                self.clear_packed_row(row);
            }
            return true;
        }

        if delta > 0 {
            for row in top..=(bottom - abs_delta) {
                self.copy_packed_row(row + abs_delta, row);
            }
            for row in (bottom - abs_delta + 1)..=bottom {
                self.clear_packed_row(row);
            }
        } else {
            for row in ((top + abs_delta)..=bottom).rev() {
                self.copy_packed_row(row - abs_delta, row);
            }
            for row in top..(top + abs_delta) {
                self.clear_packed_row(row);
            }
        }
        true
    }

    fn clear_packed_row(&mut self, row: usize) {
        let start = row * self.width;
        let end = start + self.width;
        self.cells[start..end].fill(Self::empty_cell());
        self.no_select[start..end].fill(false);
        if let Some(soft_wrap) = self.soft_wrap.get_mut(row) {
            *soft_wrap = 0;
        }
    }

    fn copy_packed_row(&mut self, src_row: usize, dst_row: usize) {
        if src_row == dst_row {
            return;
        }
        let src_start = src_row * self.width;
        let dst_start = dst_row * self.width;
        for offset in 0..self.width {
            self.cells[dst_start + offset] = self.cells[src_start + offset];
            self.no_select[dst_start + offset] = self.no_select[src_start + offset];
        }
        if let Some(src_soft_wrap) = self.soft_wrap.get(src_row).copied() {
            if let Some(dst_soft_wrap) = self.soft_wrap.get_mut(dst_row) {
                *dst_soft_wrap = src_soft_wrap;
            }
        }
    }

    fn add_diff_region(region: &mut Option<DamageRegion>, next: DamageRegion) {
        if next.width == 0 || next.height == 0 {
            return;
        }
        *region = Some(region.map_or(next, |region| region.union(next)));
    }

    fn row_damage_start(&self, y: usize) -> Option<usize> {
        let damage = self.damage_region?;
        if !damage.intersects_row(y) || damage.width == 0 {
            return None;
        }
        Some(damage.x.min(self.width))
    }

    fn cell_width_for_diff(&self, x: usize, y: usize) -> CanvasPackedCellWidth {
        self.cell(x, y)
            .map(|cell| cell.width)
            .unwrap_or(CanvasPackedCellWidth::Normal)
    }

    /// Returns the first column that must be repainted on row `y`.
    ///
    /// This is the packed-screen analogue of [`Canvas::row_change_start`] and
    /// CC Ink's damage-limited `diffEach(...)` scan. It includes previous/current
    /// damage starts, height shrink rows (which must clear stale terminal output
    /// even if the retained cells are blank), growth rows with non-empty content,
    /// and packed-cell differences. If the first changed cell is a wide tail, the
    /// start is expanded to the wide head so sparse row writers keep cursor
    /// accounting valid. The helper performs no terminal I/O.
    pub fn row_change_start(&self, next: &Self, y: usize) -> Option<usize> {
        let max_height = self.height.max(next.height);
        if y >= max_height {
            return None;
        }

        if self.width != next.width {
            return Some(0);
        }

        if y >= next.height && y < self.height {
            return Some(0);
        }

        let max_width = self.width.max(next.width);
        let damage_start = [self.row_damage_start(y), next.row_damage_start(y)]
            .into_iter()
            .flatten()
            .min();

        let diff_start = (0..max_width).find(|&x| {
            let removed = self.cell(x, y);
            let added = next.cell(x, y);
            match (removed, added) {
                (Some(previous), Some(next)) => previous != next,
                (Some(_), None) => true,
                (None, Some(next)) => !next.is_empty(),
                (None, None) => false,
            }
        });

        let mut start = match (damage_start, diff_start) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return None,
        };

        if start > 0
            && (self.cell_width_for_diff(start, y) == CanvasPackedCellWidth::WidthTail
                || next.cell_width_for_diff(start, y) == CanvasPackedCellWidth::WidthTail)
        {
            start -= 1;
        }

        Some(start.min(max_width))
    }

    fn diff_region(&self, next: &Self) -> Option<DamageRegion> {
        let mut region = None;
        if self.width == 0 && self.height == 0 {
            Self::add_diff_region(
                &mut region,
                DamageRegion {
                    x: 0,
                    y: 0,
                    width: next.width,
                    height: next.height,
                },
            );
        } else {
            if let Some(damage) = next.damage_region {
                Self::add_diff_region(&mut region, damage);
            }
            if let Some(damage) = self.damage_region {
                Self::add_diff_region(&mut region, damage);
            }
        }

        if self.height > next.height {
            Self::add_diff_region(
                &mut region,
                DamageRegion {
                    x: 0,
                    y: next.height,
                    width: self.width,
                    height: self.height - next.height,
                },
            );
        }
        if self.width > next.width {
            Self::add_diff_region(
                &mut region,
                DamageRegion {
                    x: next.width,
                    y: 0,
                    width: self.width - next.width,
                    height: self.height,
                },
            );
        }

        region
    }

    /// Returns all packed-cell differences needed to transform this screen into `next`.
    ///
    /// This mirrors CC Ink's packed `screen.diff(...)`: only the union of the
    /// previous/current damage regions plus shrink regions is scanned, additions
    /// for grown areas skip empty cells, and removals are emitted even for blank
    /// previous cells so terminal writers can clear stale output.
    pub fn diff(&self, next: &Self) -> Vec<CanvasPackedDiffChange> {
        let mut changes = Vec::new();
        self.diff_each(next, |change| {
            changes.push(change);
            false
        });
        changes
    }

    /// Calls `callback` for each packed-cell difference between this screen and `next`.
    ///
    /// The callback may return `true` to stop iteration early; this method
    /// returns whether such an early stop occurred. This is an optimization-only
    /// helper for custom retained renderers and never performs terminal I/O.
    pub fn diff_each<F>(&self, next: &Self, mut callback: F) -> bool
    where
        F: FnMut(CanvasPackedDiffChange) -> bool,
    {
        let Some(region) = self.diff_region(next) else {
            return false;
        };
        let max_height = self.height.max(next.height);
        let max_width = self.width.max(next.width);
        let start_y = region.y.min(max_height);
        let start_x = region.x.min(max_width);
        let end_y = region.y.saturating_add(region.height).min(max_height);
        let end_x = region.x.saturating_add(region.width).min(max_width);

        for y in start_y..end_y {
            for x in start_x..end_x {
                let removed = self.cell(x, y);
                let added = next.cell(x, y);
                let changed = match (removed, added) {
                    (Some(previous), Some(next)) => previous != next,
                    (Some(_), None) => true,
                    (None, Some(next)) => !next.is_empty(),
                    (None, None) => false,
                };
                if changed
                    && callback(CanvasPackedDiffChange {
                        x,
                        y,
                        removed,
                        added,
                    })
                {
                    return true;
                }
            }
        }

        false
    }

    /// Writes one packed row as ANSI without a trailing newline.
    ///
    /// This is an opt-in packed-screen counterpart to CC Ink's
    /// `renderFrameSlice(...)` sparse row writer: spacer/default cells are
    /// skipped with cursor-forward movement, style transitions are cached via
    /// [`CanvasStyleTransitionCache`], OSC 8 hyperlinks are opened/closed around
    /// linked cells, and the row is reset/erased to EOL on exit. The caller must
    /// position the terminal cursor at `start_col` before writing this fragment.
    pub fn write_ansi_row_with_style_cache<W: Write>(
        &self,
        pools: &CanvasPackedCellPools,
        style_cache: &mut CanvasStyleTransitionCache,
        y: usize,
        start_col: usize,
        mut w: W,
    ) -> io::Result<()> {
        if y >= self.height || start_col >= self.width {
            return Ok(());
        }

        let mut current_style = CanvasResolvedStyle::default();
        let mut current_style_id = 0u32;
        let mut current_hyperlink: Option<&str> = None;
        let mut last_rendered_style_id = None;
        let mut cursor_x = start_col;
        for x in start_col..self.width {
            let Some(cell) = self.visible_cell(pools, x, y, last_rendered_style_id) else {
                continue;
            };
            let view = pools.cell_view(cell);
            let cell_width = if view.width == CanvasPackedCellWidth::Wide {
                2
            } else {
                1
            };
            if cell_width == 2 && x.saturating_add(2) > self.width {
                continue;
            }

            if x > cursor_x {
                write_cursor_forward(&mut w, x - cursor_x)?;
            }

            if view.hyperlink != current_hyperlink {
                if current_hyperlink.is_some() {
                    hyperlink_close(&mut w)?;
                }
                if let Some(href) = view.hyperlink {
                    hyperlink_open(&mut w, href)?;
                }
                current_hyperlink = view.hyperlink;
            }

            let target_style = view.style.unwrap_or_default();
            let transition = style_cache.transition(current_style, target_style);
            if !transition.is_empty() {
                w.write_all(transition.as_bytes())?;
                current_style = target_style;
                current_style_id = view.style_id;
            }

            if cell_width == 2
                && packed_grapheme_needs_width_compensation(view.character)
                && x + 1 < self.width
            {
                write!(
                    w,
                    "\x1b[{}G \x1b[{}G{}\x1b[{}G",
                    x + 2,
                    x + 1,
                    view.character,
                    x + cell_width + 1
                )?;
            } else {
                w.write_all(view.character.as_bytes())?;
            }
            cursor_x = x.saturating_add(cell_width);
            last_rendered_style_id = Some(view.style_id);
        }

        if current_hyperlink.is_some() {
            hyperlink_close(&mut w)?;
        }
        let reset = style_cache.transition(current_style, CanvasResolvedStyle::default());
        if !reset.is_empty() {
            w.write_all(reset.as_bytes())?;
        } else if current_style_id != 0 {
            sgr_reset(&mut w)?;
        }
        erase_to_eol(&mut w)?;
        sgr_reset(&mut w)?;
        Ok(())
    }

    /// Returns one packed row's ANSI representation without a trailing newline.
    pub fn ansi_row_with_style_cache(
        &self,
        pools: &CanvasPackedCellPools,
        style_cache: &mut CanvasStyleTransitionCache,
        y: usize,
        start_col: usize,
    ) -> io::Result<String> {
        let mut out = Vec::new();
        self.write_ansi_row_with_style_cache(pools, style_cache, y, start_col, &mut out)?;
        String::from_utf8(out).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }
}

fn write_cursor_forward(w: &mut impl Write, cols: usize) -> io::Result<()> {
    if cols == 0 {
        return Ok(());
    }
    write!(w, "\x1b[{cols}C")
}

fn packed_grapheme_needs_width_compensation(value: &str) -> bool {
    let Some(cp) = value.chars().next().map(|ch| ch as u32) else {
        return false;
    };
    (0x1fa70..=0x1faff).contains(&cp)
        || (0x1fb00..=0x1fbff).contains(&cp)
        || (value.len() >= 2 && value.contains('\u{fe0f}'))
}

impl CanvasPackedOutputClip {
    fn intersect(self, child: Self) -> Self {
        fn max_bound(a: Option<usize>, b: Option<usize>) -> Option<usize> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
        fn min_bound(a: Option<usize>, b: Option<usize>) -> Option<usize> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }

        Self {
            x1: max_bound(self.x1, child.x1),
            x2: min_bound(self.x2, child.x2),
            y1: max_bound(self.y1, child.y1),
            y2: min_bound(self.y2, child.y2),
        }
    }
}

impl CanvasPackedOutput {
    /// Creates an empty packed output queue and reusable screen.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            screen: CanvasPackedScreen::new(width, height),
            operations: Vec::new(),
            line_cache: CanvasPackedLineCache::new(),
        }
    }

    /// Reuses this output queue for a new frame and clears queued operations.
    pub fn reset(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.operations.clear();
        self.screen.reset(width, height);
    }

    /// Returns the current target width in terminal cells.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Returns the current target height in terminal rows.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Returns the number of queued operations.
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }

    /// Returns the number of retained line-cache entries.
    pub fn line_cache_len(&self) -> usize {
        self.line_cache.len()
    }

    /// Clears the retained line cache.
    pub fn clear_line_cache(&mut self) {
        self.line_cache.clear();
    }

    /// Returns queued operations for inspection or custom planning.
    pub fn operations(&self) -> &[CanvasPackedOutputOperation] {
        &self.operations
    }

    /// Queues a single-style text write. Newlines create multiple logical rows.
    pub fn write(
        &mut self,
        x: usize,
        y: usize,
        text: impl Into<String>,
        style: CanvasResolvedStyle,
        hyperlink: Option<&str>,
        soft_wrap: Option<Vec<bool>>,
    ) {
        let hyperlink = hyperlink.map(str::to_string);
        self.write_runs(
            x,
            y,
            [CanvasPackedLineOwnedRun {
                text: text.into(),
                style,
                hyperlink,
            }],
            soft_wrap,
        );
    }

    /// Queues styled text runs. Newlines inside run text create multiple rows.
    pub fn write_runs<I>(&mut self, x: usize, y: usize, runs: I, soft_wrap: Option<Vec<bool>>)
    where
        I: IntoIterator<Item = CanvasPackedLineOwnedRun>,
    {
        let lines = split_packed_output_runs(runs);
        if lines.is_empty() {
            return;
        }
        self.operations.push(CanvasPackedOutputOperation::Write {
            x,
            y,
            lines,
            soft_wrap,
        });
    }

    /// Pushes a nested clip rectangle.
    pub fn clip(&mut self, clip: CanvasPackedOutputClip) {
        self.operations
            .push(CanvasPackedOutputOperation::Clip(clip));
    }

    /// Pops the current clip rectangle.
    pub fn unclip(&mut self) {
        self.operations.push(CanvasPackedOutputOperation::Unclip);
    }

    /// Queues a same-coordinate blit from a packed source screen.
    pub fn blit(
        &mut self,
        src: CanvasPackedScreen,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) {
        self.operations.push(CanvasPackedOutputOperation::Blit {
            src,
            x,
            y,
            width,
            height,
        });
    }

    /// Queues a damage-only clear region.
    pub fn clear(&mut self, region: DamageRegion, from_absolute: bool) {
        self.operations.push(CanvasPackedOutputOperation::Clear {
            region,
            from_absolute,
        });
    }

    /// Queues a no-select metadata region, replayed after writes and blits.
    pub fn no_select(&mut self, region: DamageRegion) {
        self.operations
            .push(CanvasPackedOutputOperation::NoSelect { region });
    }

    /// Queues a full-width packed row shift.
    pub fn shift(&mut self, top: usize, bottom: usize, delta: i32) {
        self.operations
            .push(CanvasPackedOutputOperation::Shift { top, bottom, delta });
    }

    /// Applies queued operations to the reusable packed screen and returns it.
    pub fn get(&mut self, pools: &mut CanvasPackedCellPools) -> &CanvasPackedScreen {
        self.screen.reset(self.width, self.height);
        let operations = self.operations.clone();

        let mut absolute_clears = Vec::new();
        for operation in &operations {
            let CanvasPackedOutputOperation::Clear {
                region,
                from_absolute,
            } = operation
            else {
                continue;
            };
            let Some(region) = clip_damage_region(*region, self.width, self.height) else {
                continue;
            };
            self.screen.mark_damage(region);
            if *from_absolute {
                absolute_clears.push(region);
            }
        }

        let mut clips = Vec::<CanvasPackedOutputClip>::new();
        for operation in &operations {
            match operation {
                CanvasPackedOutputOperation::Clear { .. } => {}
                CanvasPackedOutputOperation::Clip(clip) => {
                    let next = clips
                        .last()
                        .copied()
                        .map_or(*clip, |parent| parent.intersect(*clip));
                    clips.push(next);
                }
                CanvasPackedOutputOperation::Unclip => {
                    clips.pop();
                }
                CanvasPackedOutputOperation::Blit {
                    src,
                    x,
                    y,
                    width,
                    height,
                } => {
                    let Some((start_x, start_y, copy_width, copy_height)) = clip_packed_region(
                        *x,
                        *y,
                        *width,
                        *height,
                        self.width,
                        self.height,
                        src.width,
                        src.height,
                        clips.last().copied(),
                    ) else {
                        continue;
                    };
                    self.screen.blit_region_from_excluding_clears(
                        src,
                        start_x,
                        start_y,
                        copy_width,
                        copy_height,
                        &absolute_clears,
                    );
                }
                CanvasPackedOutputOperation::Shift { top, bottom, delta } => {
                    self.screen.shift_rows(*top, *bottom, *delta);
                }
                CanvasPackedOutputOperation::Write {
                    x,
                    y,
                    lines,
                    soft_wrap,
                } => {
                    self.apply_write_operation(
                        pools,
                        *x,
                        *y,
                        lines,
                        soft_wrap.as_deref(),
                        clips.last().copied(),
                    );
                }
                CanvasPackedOutputOperation::NoSelect { .. } => {}
            }
        }

        for operation in &operations {
            if let CanvasPackedOutputOperation::NoSelect { region } = operation {
                self.screen
                    .mark_no_select_region(region.x, region.y, region.width, region.height);
            }
        }

        &self.screen
    }

    fn apply_write_operation(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        lines: &[Vec<CanvasPackedLineOwnedRun>],
        soft_wrap: Option<&[bool]>,
        clip: Option<CanvasPackedOutputClip>,
    ) {
        let min_x = clip.and_then(|clip| clip.x1).unwrap_or(0).min(self.width);
        let max_x = clip
            .and_then(|clip| clip.x2)
            .unwrap_or(self.width)
            .min(self.width);
        let min_y = clip.and_then(|clip| clip.y1).unwrap_or(0).min(self.height);
        let max_y = clip
            .and_then(|clip| clip.y2)
            .unwrap_or(self.height)
            .min(self.height);
        if min_x >= max_x || min_y >= max_y || y >= self.height || lines.is_empty() {
            return;
        }

        let from = min_y.saturating_sub(y).min(lines.len());
        let to = lines.len().min(max_y.saturating_sub(y));
        if from >= to {
            return;
        }

        let mut prev_content_end = 0usize;
        if let Some(soft_wrap) = soft_wrap {
            if from > 0 && soft_wrap.get(from).copied() == Some(true) {
                prev_content_end =
                    measure_packed_output_line_end(&mut self.line_cache, x, &lines[from - 1]);
            }
        }

        for (offset_y, line) in lines[from..to].iter().enumerate() {
            let line_idx = from + offset_y;
            let line_y = y + line_idx;
            let content_end = self.write_output_line(pools, x, line_y, line, min_x, max_x);
            if let Some(soft_wrap) = soft_wrap {
                if let Some(slot) = self.screen.soft_wrap.get_mut(line_y) {
                    *slot = if soft_wrap.get(line_idx).copied() == Some(true) {
                        prev_content_end.min(self.width)
                    } else {
                        0
                    };
                }
                prev_content_end = content_end;
            }
        }
    }

    fn write_output_line(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        line: &[CanvasPackedLineOwnedRun],
        min_x: usize,
        max_x: usize,
    ) -> usize {
        let mut offset_x = x;
        for run in line {
            let style_id = pools.intern_style(run.style);
            let hyperlink_id = pools.intern_hyperlink(run.hyperlink.as_deref());
            let clusters = self.line_cache.clusters(&run.text);
            offset_x = self.screen.write_cached_clusters_clipped_with_ids(
                pools,
                clusters,
                offset_x,
                y,
                style_id,
                hyperlink_id,
                min_x,
                max_x,
            );
        }
        offset_x
    }
}

fn split_packed_output_runs<I>(runs: I) -> Vec<Vec<CanvasPackedLineOwnedRun>>
where
    I: IntoIterator<Item = CanvasPackedLineOwnedRun>,
{
    let mut lines: Vec<Vec<CanvasPackedLineOwnedRun>> = vec![Vec::new()];
    for run in runs {
        let parts = run.text.split('\n').collect::<Vec<_>>();
        for (idx, part) in parts.iter().enumerate() {
            if idx > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines.last_mut().unwrap().push(CanvasPackedLineOwnedRun {
                    text: (*part).to_string(),
                    style: run.style,
                    hyperlink: run.hyperlink.clone(),
                });
            }
        }
    }
    lines
}

fn measure_packed_output_line_end(
    cache: &mut CanvasPackedLineCache,
    x: usize,
    line: &[CanvasPackedLineOwnedRun],
) -> usize {
    let mut offset_x = x;
    for run in line {
        let clusters = cache.clusters(&run.text);
        offset_x = measure_packed_clusters_end(clusters, offset_x);
    }
    offset_x
}

fn measure_packed_clusters_end(clusters: &[CanvasPackedLineCluster], mut offset_x: usize) -> usize {
    let mut idx = 0;
    while idx < clusters.len() {
        let cluster = &clusters[idx];
        if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
            if code == b'\t' {
                offset_x = offset_x.saturating_add(8 - (offset_x % 8));
                idx += 1;
            } else if code == 0x1b {
                idx = skip_escape_sequence_packed_clusters(clusters, idx);
            } else {
                idx += 1;
            }
            continue;
        }
        offset_x = offset_x.saturating_add(cluster.width);
        idx += 1;
    }
    offset_x
}

fn clip_damage_region(region: DamageRegion, width: usize, height: usize) -> Option<DamageRegion> {
    let start_x = region.x.min(width);
    let start_y = region.y.min(height);
    let max_x = region.x.saturating_add(region.width).min(width);
    let max_y = region.y.saturating_add(region.height).min(height);
    (start_x < max_x && start_y < max_y).then_some(DamageRegion {
        x: start_x,
        y: start_y,
        width: max_x - start_x,
        height: max_y - start_y,
    })
}

fn clip_packed_region(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    dst_width: usize,
    dst_height: usize,
    src_width: usize,
    src_height: usize,
    clip: Option<CanvasPackedOutputClip>,
) -> Option<(usize, usize, usize, usize)> {
    let start_x = x
        .max(clip.and_then(|clip| clip.x1).unwrap_or(0))
        .min(dst_width)
        .min(src_width);
    let start_y = y
        .max(clip.and_then(|clip| clip.y1).unwrap_or(0))
        .min(dst_height)
        .min(src_height);
    let max_x = x
        .saturating_add(width)
        .min(clip.and_then(|clip| clip.x2).unwrap_or(usize::MAX))
        .min(dst_width)
        .min(src_width);
    let max_y = y
        .saturating_add(height)
        .min(clip.and_then(|clip| clip.y2).unwrap_or(usize::MAX))
        .min(dst_height)
        .min(src_height);
    (start_x < max_x && start_y < max_y).then_some((
        start_x,
        start_y,
        max_x - start_x,
        max_y - start_y,
    ))
}
