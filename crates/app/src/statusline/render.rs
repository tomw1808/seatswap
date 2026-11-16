//! The status line itself: one layout of segments, coloured, fitted to a
//! terminal width (spec §5.1, §5.4; step B, Task 2).
//!
//! Everything here is pure: the numbers come in as [`Facts`], the layout from
//! the stored [`StatuslineConfig`], and the time zone is a parameter so the
//! tests can fix an offset. No file is read except the repository's `HEAD` for
//! the branch.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Serialize, Serializer};

use seatswap_core::project::{git_dir, repository_root};
use seatswap_core::store::{ColorRule, Segment, SegmentKind, StatuslineConfig, TimeFormat};

use super::Payload;
use crate::format::{
    compact_left, format_reset_at, format_reset_clock, integer_percent, letter_color, parse_color,
    with_tilde,
};

/// The named terminal colours, one table: the name the preview uses, the SGR
/// code `ansi` writes as a foreground, the code it writes as a background, and
/// the xterm default `#rrggbb` the page paints.
const NAMED_COLORS: [(&str, &str, &str, &str); 7] = [
    ("green", "\x1b[32m", "\x1b[42m", "#00cd00"),
    ("yellow", "\x1b[33m", "\x1b[43m", "#cdcd00"),
    ("red", "\x1b[31m", "\x1b[41m", "#cd0000"),
    ("magenta", "\x1b[35m", "\x1b[45m", "#cd00cd"),
    ("cyan", "\x1b[36m", "\x1b[46m", "#00cdcd"),
    ("bright yellow", "\x1b[93m", "\x1b[103m", "#ffff00"),
    ("bright red", "\x1b[91m", "\x1b[101m", "#ff0000"),
];

const GREEN: NamedColor = NamedColor::Green;
const YELLOW: NamedColor = NamedColor::Yellow;
const RED: NamedColor = NamedColor::Red;
const MAGENTA: NamedColor = NamedColor::Magenta;
const CYAN: NamedColor = NamedColor::Cyan;
const BRIGHT_YELLOW: NamedColor = NamedColor::BrightYellow;
const BRIGHT_RED: NamedColor = NamedColor::BrightRed;

const RESET: &str = "\x1b[0m";

/// One of the named colours of the table [`NAMED_COLORS`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NamedColor {
    Green,
    Yellow,
    Red,
    Magenta,
    Cyan,
    BrightYellow,
    BrightRed,
}

impl NamedColor {
    /// The row of this colour in the one table.
    fn row(self) -> (&'static str, &'static str, &'static str, &'static str) {
        NAMED_COLORS[match self {
            NamedColor::Green => 0,
            NamedColor::Yellow => 1,
            NamedColor::Red => 2,
            NamedColor::Magenta => 3,
            NamedColor::Cyan => 4,
            NamedColor::BrightYellow => 5,
            NamedColor::BrightRed => 6,
        }]
    }
}

/// A span's colour: a named terminal colour, or an RGB colour from the store.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Color {
    Named(NamedColor),
    Rgb([u8; 3]),
}

impl Color {
    /// The SGR sequence `ansi` writes for this colour.
    fn sgr(self, background: bool) -> String {
        match self {
            Color::Named(named) => {
                let (_, foreground, background_sgr, _) = named.row();
                if background {
                    background_sgr.to_string()
                } else {
                    foreground.to_string()
                }
            }
            Color::Rgb([red, green, blue]) => {
                let layer = if background { 48 } else { 38 };
                format!("\x1b[{layer};2;{red};{green};{blue}m")
            }
        }
    }

    /// The `#rrggbb` the page paints for this colour.
    fn hex(self) -> String {
        match self {
            Color::Named(named) => named.row().3.to_string(),
            Color::Rgb([red, green, blue]) => format!("#{red:02x}{green:02x}{blue:02x}"),
        }
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

/// One piece of a fitted line. `fg` and `bg` serialise as `#rrggbb`, while
/// [`ansi`] keeps writing the old SGR bytes.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Span {
    pub text: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
}

/// The payload field paths of the optional segments: the one table the renderer
/// and the window's `statusline_fields` command both read. A kind that reads no
/// payload field has no row.
const FIELD_PATHS: [(SegmentKind, &[&str]); 3] = [
    (
        SegmentKind::ContextUsed,
        &["context_window.used_percentage"],
    ),
    (SegmentKind::SessionCost, &["cost.total_cost_usd"]),
    (
        SegmentKind::LinesChanged,
        &["cost.total_lines_added", "cost.total_lines_removed"],
    ),
];

/// One optional segment and the payload paths it reads, for the page.
#[cfg(any(feature = "gui", test))]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FieldPaths {
    pub kind: SegmentKind,
    pub paths: &'static [&'static str],
}

/// The field paths `kind` reads; empty for a kind that reads no payload field.
pub(crate) fn field_paths(kind: SegmentKind) -> &'static [&'static str] {
    for (entry, paths) in FIELD_PATHS {
        if entry == kind {
            return paths;
        }
    }
    &[]
}

/// Every kind that reads a payload field, in the table's order.
#[cfg(any(feature = "gui", test))]
pub(crate) fn all_field_paths() -> Vec<FieldPaths> {
    FIELD_PATHS
        .iter()
        .map(|(kind, paths)| FieldPaths { kind: *kind, paths })
        .collect()
}

/// The value at a dot-separated path of the raw payload, or `None` when a
/// parent is absent or not an object.
pub(crate) fn at_path<'a>(raw: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut value = raw;
    for key in path.split('.') {
        value = value.get(key)?;
    }
    Some(value)
}

/// What the line shows besides the payload's model, effort and folder.
#[derive(Clone)]
pub(crate) struct Facts {
    pub user: String,
    pub host: String,
    pub home: String,
    pub now: DateTime<Utc>,
    pub account: Option<AccountFacts>,
    pub five_hour: Option<Limit2>,
    pub week: Option<Limit2>,
    pub model_limits: Vec<(String, f64)>,
    pub burn_next: Option<(String, DateTime<Utc>)>,
    pub git_branch: Option<String>,
}

/// The live account's short text and colours.
#[derive(Clone)]
pub(crate) struct AccountFacts {
    pub abbr: String,
    pub color: String,
    pub plan: Option<String>,
}

/// One limit: a percent and its reset, when the source has one.
#[derive(Clone, Copy)]
pub(crate) struct Limit2 {
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// A segment that has a value, ready to colour and place. `prefix` is kept
/// apart from `text` because a prefix's leading spaces are dropped when the
/// text before it ends in a space and a segment between them was left out: a
/// missing segment leaves no gap.
struct Piece {
    /// Only the preview's note names a dropped kind.
    #[cfg(any(feature = "gui", test))]
    kind: SegmentKind,
    prefix: String,
    text: String,
    suffix: String,
    fg: Option<Color>,
    bg: Option<Color>,
    drop: Option<u8>,
    /// At least one segment between the previous piece and this one in the
    /// configuration has no value. Together with a segment dropped by the
    /// fitting, this is what makes a prefix's leading spaces go away.
    missing_before: bool,
}

/// A fitted line and what the fitting removed: the drop numbers and the kinds
/// of the pieces they removed, ordered by the drop number that removed them and
/// then by their place in the line.
pub(crate) struct FittedLine {
    pub spans: Vec<Span>,
    pub dropped: Vec<u8>,
    /// The preview's note reads this; the command renders without it.
    #[cfg(any(feature = "gui", test))]
    pub dropped_kinds: Vec<SegmentKind>,
}

/// The fitted line as spans: left spans, one span of spaces for the gap, right
/// spans. `width = None`: the right part follows the left after the gap (two
/// spaces when both zones print something, none when one is empty) and nothing
/// is dropped. With a width the right part is flush with `width - margin`, and
/// segments are dropped by their `drop` number until the line fits; when
/// nothing is left to drop it is printed anyway. The second value is the drop
/// numbers removed.
pub(crate) fn render_spans<Tz: TimeZone>(
    config: &StatuslineConfig,
    payload: &Payload,
    facts: &Facts,
    width: Option<u16>,
    tz: &Tz,
) -> (Vec<Span>, Vec<u8>)
where
    Tz::Offset: std::fmt::Display,
{
    let fitted = fitted_line(config, payload, facts, width, tz);
    (fitted.spans, fitted.dropped)
}

/// The one fitting rule: the fitted line with the drop numbers it removed and
/// the kinds of the pieces those numbers removed.
pub(crate) fn fitted_line<Tz: TimeZone>(
    config: &StatuslineConfig,
    payload: &Payload,
    facts: &Facts,
    width: Option<u16>,
    tz: &Tz,
) -> FittedLine
where
    Tz::Offset: std::fmt::Display,
{
    let left = pieces(&config.left, payload, facts, tz);
    let right = pieces(&config.right, payload, facts, tz);

    let mut level: Option<u8> = None;
    if let Some(width) = width {
        let mut drops: Vec<u8> = left
            .iter()
            .chain(right.iter())
            .filter_map(|piece| piece.drop)
            .collect();
        drops.sort_unstable();
        drops.dedup();
        loop {
            if fits(&left, &right, level, config.margin, width) {
                break;
            }
            match drops
                .iter()
                .find(|drop| level.is_none_or(|current| **drop > current))
            {
                Some(next) => level = Some(*next),
                None => break,
            }
        }
    }

    let dropped: Vec<u8> = match level {
        Some(level) => {
            let mut dropped: Vec<u8> = left
                .iter()
                .chain(right.iter())
                .filter_map(|piece| piece.drop)
                .filter(|drop| *drop <= level)
                .collect();
            dropped.sort_unstable();
            dropped.dedup();
            dropped
        }
        None => Vec::new(),
    };
    #[cfg(any(feature = "gui", test))]
    let dropped_kinds: Vec<SegmentKind> = match level {
        Some(level) => {
            let mut removed: Vec<(u8, SegmentKind)> = left
                .iter()
                .chain(right.iter())
                .filter_map(|piece| {
                    piece
                        .drop
                        .filter(|drop| *drop <= level)
                        .map(|drop| (drop, piece.kind))
                })
                .collect();
            removed.sort_by_key(|(drop, _)| *drop);
            removed.into_iter().map(|(_, kind)| kind).collect()
        }
        None => Vec::new(),
    };

    let left_spans = compose_zone(&left, level);
    let right_spans = compose_zone(&right, level);
    let left_width = spans_width(&left_spans);
    let right_width = spans_width(&right_spans);
    if right_spans.is_empty() {
        return FittedLine {
            spans: left_spans,
            dropped,
            #[cfg(any(feature = "gui", test))]
            dropped_kinds,
        };
    }
    let min_gap = zone_gap(left_width, right_width);
    let gap = match width {
        Some(width) => {
            let target = usize::from(width).saturating_sub(usize::from(config.margin));
            target.saturating_sub(left_width + right_width).max(min_gap)
        }
        None => min_gap,
    };
    let mut spans = left_spans;
    if gap > 0 {
        spans.push(Span {
            text: " ".repeat(gap),
            fg: None,
            bg: None,
            bold: false,
        });
    }
    spans.extend(right_spans);
    FittedLine {
        spans,
        dropped,
        #[cfg(any(feature = "gui", test))]
        dropped_kinds,
    }
}

/// The spans as the terminal line: one SGR run per stretch of spans with the
/// same style, closed by a reset. Neighbouring spans with the same style are
/// written as one run, so a value and its suffix that share a foreground are
/// one coloured run. A segment's prefix is always plain, and a suffix is plain
/// when the value has a background, so a background covers the value only.
pub(crate) fn ansi(spans: &[Span]) -> String {
    let mut out = String::new();
    let mut index = 0;
    while index < spans.len() {
        let span = &spans[index];
        if span.fg.is_none() && span.bg.is_none() && !span.bold {
            out.push_str(&span.text);
            index += 1;
            continue;
        }
        let (fg, bg, bold) = (span.fg, span.bg, span.bold);
        if bold {
            out.push_str("\x1b[1m");
        }
        if let Some(background) = bg {
            out.push_str(&background.sgr(true));
        }
        if let Some(foreground) = fg {
            out.push_str(&foreground.sgr(false));
        }
        out.push_str(&span.text);
        index += 1;
        while index < spans.len() {
            let next = &spans[index];
            if next.fg != fg || next.bg != bg || next.bold != bold {
                break;
            }
            out.push_str(&next.text);
            index += 1;
        }
        out.push_str(RESET);
    }
    out
}

/// The printed width of a list of spans.
fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|span| visible_width(&span.text)).sum()
}

/// The two spaces between the zones, when there are two zones to separate. An
/// empty zone takes no gap, so the other zone can sit flush against the margin.
fn zone_gap(left_width: usize, right_width: usize) -> usize {
    if left_width > 0 && right_width > 0 {
        2
    } else {
        0
    }
}

/// Whether the line built from the segments above `level` keeps the margin and
/// the gap the two zones need.
fn fits(left: &[Piece], right: &[Piece], level: Option<u8>, margin: u16, width: u16) -> bool {
    let left = spans_width(&compose_zone(left, level));
    let right = spans_width(&compose_zone(right, level));
    left + zone_gap(left, right) + right + usize::from(margin) <= usize::from(width)
}

/// `text` without its ANSI escape sequences.
fn visible_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // CSI: ESC '[' … one byte in 0x40..=0x7e.
            let mut rest = chars.clone();
            if rest.next() == Some('[') {
                let mut skipped = false;
                for next in rest.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        skipped = true;
                        break;
                    }
                }
                if skipped {
                    chars = rest;
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}

/// The printed width of `text`, in columns: its characters without the ANSI
/// escape sequences, each counted by [`char_width`].
pub(crate) fn visible_width(text: &str) -> usize {
    visible_text(text).chars().map(char_width).sum()
}

/// The East Asian wide and fullwidth ranges and the emoji ranges: two columns.
const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115f),
    (0x2e80, 0x303e),
    (0x3041, 0x33ff),
    (0x3400, 0x4dbf),
    (0x4e00, 0x9fff),
    (0xa000, 0xa4cf),
    (0xac00, 0xd7a3),
    (0xf900, 0xfaff),
    (0xfe30, 0xfe4f),
    (0xff00, 0xff60),
    (0xffe0, 0xffe6),
    (0x1f300, 0x1f64f),
    (0x1f900, 0x1f9ff),
    (0x20000, 0x2fffd),
    (0x30000, 0x3fffd),
];

/// The combining marks and zero-width format characters: no column.
const ZERO_WIDTH: &[(u32, u32)] = &[
    (0x0300, 0x036f),
    (0x1ab0, 0x1aff),
    (0x1dc0, 0x1dff),
    (0x20d0, 0x20ff),
    (0xfe20, 0xfe2f),
    (0x200b, 0x200f),
    (0xfe00, 0xfe0f),
];

/// The columns one code point takes: two in the East Asian wide, fullwidth and
/// emoji ranges, none for a combining mark or a zero-width format character,
/// one for everything else. This is an approximation of the Unicode width
/// tables, kept without a dependency; a code point outside the tables above is
/// counted as one column.
fn char_width(ch: char) -> usize {
    let code = u32::from(ch);
    if in_ranges(WIDE, code) {
        2
    } else if in_ranges(ZERO_WIDTH, code) {
        0
    } else {
        1
    }
}

fn in_ranges(ranges: &[(u32, u32)], code: u32) -> bool {
    ranges
        .iter()
        .any(|(first, last)| (*first..=*last).contains(&code))
}

/// The branch of the repository `cwd` belongs to, from its git dir's `HEAD`.
/// The repository is found with core's walk ([`repository_root`]), so a folder
/// below the repository root and a worktree (a `.git` file) find their branch,
/// and [`git_dir`] follows a `.git` file to the folder holding `HEAD`. A
/// detached HEAD (a commit hash) has none. Nothing is executed.
pub(crate) fn git_branch(cwd: &Path, home: &Path) -> Option<String> {
    let root = repository_root(cwd, home)?;
    let gitdir = git_dir(&root.join(".git"))?;
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim_end_matches(['\n', '\r']);
    let name = head.strip_prefix("ref: refs/heads/")?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// The segments of one zone that have a value, in layout order. A segment
/// without a value is left out of the list; every piece after it records that
/// one segment was left out (`missing_before`), so the fitting and the prefix
/// rule can tell a gap from two segments that are adjacent in the configuration.
fn pieces<Tz: TimeZone>(
    segments: &[Segment],
    payload: &Payload,
    facts: &Facts,
    tz: &Tz,
) -> Vec<Piece>
where
    Tz::Offset: std::fmt::Display,
{
    let mut pieces = Vec::new();
    let mut missing = false;
    for segment in segments {
        let Some(text) = value(segment, payload, facts, tz) else {
            missing = true;
            continue;
        };
        pieces.push(Piece {
            #[cfg(any(feature = "gui", test))]
            kind: segment.kind,
            prefix: segment.prefix.clone(),
            text,
            suffix: segment.suffix.clone(),
            fg: color(segment, payload, facts),
            bg: background(segment, facts),
            drop: segment.drop,
            missing_before: missing,
        });
        missing = false;
    }
    pieces
}

/// The visible text of one segment, or `None` when its value is missing.
fn value<Tz: TimeZone>(
    segment: &Segment,
    payload: &Payload,
    facts: &Facts,
    tz: &Tz,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    match segment.kind {
        SegmentKind::Account => facts.account.as_ref().map(|account| account.abbr.clone()),
        SegmentKind::Plan => facts
            .account
            .as_ref()
            .and_then(|account| account.plan.clone()),
        SegmentKind::FiveHourPercent => facts
            .five_hour
            .as_ref()
            .map(|limit| integer_percent(limit.percent).to_string()),
        SegmentKind::FiveHourReset => facts.five_hour.as_ref().and_then(|limit| {
            limit
                .resets_at
                .map(|resets_at| reset_text(segment.time_format, resets_at, facts.now, false, tz))
        }),
        SegmentKind::WeekPercent => facts
            .week
            .as_ref()
            .map(|limit| integer_percent(limit.percent).to_string()),
        SegmentKind::WeekReset => facts.week.as_ref().and_then(|limit| {
            limit
                .resets_at
                .map(|resets_at| reset_text(segment.time_format, resets_at, facts.now, true, tz))
        }),
        SegmentKind::ModelPercent => {
            if facts.model_limits.is_empty() {
                None
            } else {
                Some(
                    facts
                        .model_limits
                        .iter()
                        .map(|(model, percent)| format!("{model} {}%", integer_percent(*percent)))
                        .collect::<Vec<String>>()
                        .join("  "),
                )
            }
        }
        SegmentKind::AutoBurn => facts
            .burn_next
            .as_ref()
            .map(|(account, at)| format!("{account} {}", format_reset_at(*at, facts.now, tz))),
        SegmentKind::Model => payload
            .model
            .as_ref()
            .and_then(|model| model.display_name.clone()),
        SegmentKind::Effort => payload
            .effort
            .as_ref()
            .and_then(|effort| effort.level.clone()),
        SegmentKind::Folder => payload
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.current_dir.as_deref())
            .map(|folder| with_tilde(folder, Path::new(&facts.home))),
        SegmentKind::GitBranch => facts.git_branch.clone(),
        SegmentKind::UserHost => Some(format!("{}@{}", facts.user, facts.host)),
        SegmentKind::Clock => Some(facts.now.with_timezone(tz).format("%H:%M").to_string()),
        SegmentKind::Text => segment
            .text
            .as_ref()
            .filter(|text| !text.is_empty())
            .cloned(),
        SegmentKind::Separator => Some("|".to_string()),
        SegmentKind::ContextUsed => field_paths(SegmentKind::ContextUsed)
            .first()
            .and_then(|path| number_at(payload, path))
            .map(|value| format!("{}%", whole(value))),
        SegmentKind::SessionCost => field_paths(SegmentKind::SessionCost)
            .first()
            .and_then(|path| number_at(payload, path))
            .map(|value| format!("${value:.2}")),
        SegmentKind::LinesChanged => {
            let paths = field_paths(SegmentKind::LinesChanged);
            let added = number_at(payload, paths.first()?)?;
            let removed = number_at(payload, paths.get(1)?)?;
            Some(format!("+{} -{}", whole(added), whole(removed)))
        }
    }
}

/// The number at a payload path; absent, `null` and every other JSON type are
/// `None`, so the segment prints nothing.
fn number_at(payload: &Payload, path: &str) -> Option<f64> {
    at_path(&payload.raw, path).and_then(serde_json::Value::as_f64)
}

/// A number as an integer: exactly when it is whole, else rounded the way
/// [`integer_percent`] rounds.
fn whole(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        integer_percent(value).to_string()
    }
}

/// A reset time in the segment's format. A reset segment without a format uses
/// seatswap's own format, the one the rest of the program shows.
fn reset_text<Tz: TimeZone>(
    format: Option<TimeFormat>,
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
    week: bool,
    tz: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match format.unwrap_or(TimeFormat::Seatswap) {
        TimeFormat::CompactLeft => compact_left((resets_at - now).num_seconds(), week),
        TimeFormat::Seatswap => format_reset_at(resets_at, now, tz),
        TimeFormat::Clock => format_reset_clock(resets_at, now, tz),
    }
}

/// The foreground colour of a segment's colour rule, if it has one.
fn color(segment: &Segment, payload: &Payload, facts: &Facts) -> Option<Color> {
    match &segment.color {
        ColorRule::None => None,
        ColorRule::Fixed { color } => {
            let [red, green, blue] = parse_color(color)?;
            Some(Color::Rgb([red, green, blue]))
        }
        ColorRule::ByLoad { warn, bad } => {
            let percent = load_percent(segment.kind, facts)?;
            if percent >= f64::from(*bad) {
                Some(Color::Named(RED))
            } else if percent >= f64::from(*warn) {
                Some(Color::Named(YELLOW))
            } else {
                Some(Color::Named(GREEN))
            }
        }
        ColorRule::ByModelFamily => {
            let id = payload
                .model
                .as_ref()
                .and_then(|model| model.id.as_deref())?
                .to_ascii_lowercase();
            if id.contains("opus") {
                Some(Color::Named(MAGENTA))
            } else if id.contains("sonnet") {
                Some(Color::Named(CYAN))
            } else if id.contains("haiku") {
                Some(Color::Named(GREEN))
            } else {
                Some(Color::Named(YELLOW))
            }
        }
        ColorRule::ByEffort => {
            let level = payload
                .effort
                .as_ref()
                .and_then(|effort| effort.level.as_deref())?;
            match level {
                "low" => Some(Color::Named(GREEN)),
                "medium" => Some(Color::Named(YELLOW)),
                "high" => Some(Color::Named(BRIGHT_YELLOW)),
                "xhigh" => Some(Color::Named(RED)),
                "max" => Some(Color::Named(BRIGHT_RED)),
                _ => None,
            }
        }
        ColorRule::Account => {
            let [red, green, blue] = parse_color(&facts.account.as_ref()?.color)?;
            let [text_red, text_green, text_blue] = letter_color([red, green, blue]);
            Some(Color::Rgb([text_red, text_green, text_blue]))
        }
    }
}

/// The background colour of a segment's colour rule, if it has one. Only the
/// account tag paints one today.
fn background(segment: &Segment, facts: &Facts) -> Option<Color> {
    match &segment.color {
        ColorRule::Account => {
            let [red, green, blue] = parse_color(&facts.account.as_ref()?.color)?;
            Some(Color::Rgb([red, green, blue]))
        }
        _ => None,
    }
}

/// The percentage a `ByLoad` segment is coloured by.
fn load_percent(kind: SegmentKind, facts: &Facts) -> Option<f64> {
    match kind {
        SegmentKind::FiveHourPercent => facts.five_hour.as_ref().map(|limit| limit.percent),
        SegmentKind::WeekPercent => facts.week.as_ref().map(|limit| limit.percent),
        SegmentKind::ModelPercent => facts.model_limits.first().map(|(_, percent)| *percent),
        _ => None,
    }
}

/// One zone as it prints with the drop `level`: every segment whose drop
/// number is above `level` (or has none) stays. A segment's prefix loses its
/// leading spaces only when the text before it ends in a space and at least one
/// segment between them was left out (no value, or dropped by the fitting), so
/// a gap leaves no leading spaces while two segments adjacent in the
/// configuration print their suffix and prefix as written.
///
/// A segment becomes up to three spans: its prefix, its value and its suffix.
/// Only the value carries the colour rule's background, so the account tag's
/// background covers its letters and not the space after them. A prefix is
/// plain. A suffix keeps the value's foreground when the rule paints no
/// background (the unit "%" stays the value's colour, and `ansi` writes the
/// same run as before); with a background, the suffix is plain too, so the
/// colour stops at the value. The visible text and its width are the same as
/// one span's would be.
fn compose_zone(pieces: &[Piece], level: Option<u8>) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut ends_space = false;
    let mut left_out = false;
    for piece in pieces {
        if piece
            .drop
            .is_some_and(|drop| level.is_some_and(|current| drop <= current))
        {
            left_out = true;
            continue;
        }
        let prefix = if ends_space && (left_out || piece.missing_before) {
            piece.prefix.trim_start_matches(' ')
        } else {
            &piece.prefix
        };
        let text = format!("{prefix}{}{}", piece.text, piece.suffix);
        ends_space = text.ends_with(' ');
        left_out = false;
        if !prefix.is_empty() {
            spans.push(Span {
                text: prefix.to_string(),
                fg: None,
                bg: None,
                bold: false,
            });
        }
        if !piece.text.is_empty() {
            spans.push(Span {
                text: piece.text.clone(),
                fg: piece.fg,
                bg: piece.bg,
                bold: false,
            });
        }
        if !piece.suffix.is_empty() {
            let fg = if piece.bg.is_some() { None } else { piece.fg };
            spans.push(Span {
                text: piece.suffix.clone(),
                fg,
                bg: None,
                bold: false,
            });
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::FixedOffset;
    use seatswap_core::store::StatuslineConfig;

    /// 4 h 45 min before the 5 h reset, 3 d 16 h before the weekly one.
    const NOW: i64 = 1_794_473_100;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(seconds, 0).expect("a valid test timestamp")
    }

    /// The status line payload of `specs/plan/README.md` §5, with the folder the
    /// Task 2 tests name, below the platform's made-up home folder.
    fn payload() -> Payload {
        let raw = serde_json::json!({
            "workspace": {
                "current_dir": crate::test_support::at_home("projects/seatswap/crates/app/ui")
                    .display()
                    .to_string()
            },
            "model": {"id": "claude-fable-5-2", "display_name": "Fable 5.2"},
            "effort": {"level": "high"},
            "rate_limits": {
                "five_hour": {"used_percentage": 12, "resets_at": 1794490200},
                "seven_day": {"used_percentage": 48, "resets_at": 1794790800}
            }
        });
        super::super::parse(raw.to_string().as_bytes())
    }

    fn facts() -> Facts {
        Facts {
            user: "sam".to_string(),
            host: "atlas".to_string(),
            home: crate::test_support::home().display().to_string(),
            now: at(NOW),
            account: Some(AccountFacts {
                abbr: "AC".to_string(),
                color: "#3b82f6".to_string(),
                plan: None,
            }),
            five_hour: Some(Limit2 {
                percent: 12.0,
                resets_at: Some(at(1_794_490_200)),
            }),
            week: Some(Limit2 {
                percent: 48.0,
                resets_at: Some(at(1_794_790_800)),
            }),
            model_limits: Vec::new(),
            burn_next: None,
            git_branch: None,
        }
    }

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(3_600).expect("a fixed offset")
    }

    /// The old `render`, now the two steps the command uses.
    fn rendered<Tz: TimeZone>(
        config: &StatuslineConfig,
        payload: &Payload,
        facts: &Facts,
        width: Option<u16>,
        tz: &Tz,
    ) -> String
    where
        Tz::Offset: std::fmt::Display,
    {
        ansi(&render_spans(config, payload, facts, width, tz).0)
    }

    #[test]
    fn the_current_line_at_120_columns() {
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(120),
            &offset(),
        );
        // 58 columns left, 23 right, 35 spaces between them; the margin of 4
        // stays free at the right edge.
        assert_eq!(
            visible_text(&line),
            format!(
                "sam@atlas:{} Fable 5.2 high{}AC 12% 4h45m  48% 3d16h",
                crate::test_support::tilde("projects/seatswap/crates/app/ui"),
                " ".repeat(35)
            )
        );
        assert_eq!(visible_width(&line), 116);
    }

    #[test]
    fn at_80_columns_times_then_the_five_hour_percent_are_dropped() {
        // The plan lists `AC 48%` here. Its own fitting rule
        // (left 58 + 2 + right 11 + margin 4 = 75 <= 80) stops after the two
        // reset times; the plan's value follows only when the folder is not
        // shortened (left 66, 66 + 2 + 11 + 4 = 83 > 80). The rule is kept and
        // the value reported, not forced.
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(80),
            &offset(),
        );
        assert!(visible_text(&line).ends_with("AC 12%  48%"), "{line}");

        // The next drop number removes the 5 h percent too, and the week
        // percent's separator leaves no gap after it.
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(74),
            &offset(),
        );
        assert!(visible_text(&line).ends_with("AC 48%"), "{line}");
    }

    #[test]
    fn without_a_width_the_right_part_follows_after_two_spaces() {
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            None,
            &offset(),
        );
        assert_eq!(
            visible_text(&line),
            format!(
                "sam@atlas:{} Fable 5.2 high  AC 12% 4h45m  48% 3d16h",
                crate::test_support::tilde("projects/seatswap/crates/app/ui")
            )
        );
    }

    #[test]
    fn narrower_than_the_left_part_still_prints() {
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(20),
            &offset(),
        );
        let text = visible_text(&line);
        assert!(text.starts_with("sam@atlas:"), "{text}");
        assert!(text.contains("AC 48%"), "{text}");
    }

    #[test]
    fn no_session_limit_leaves_no_gap() {
        let mut facts = facts();
        facts.five_hour = None;
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts,
            None,
            &offset(),
        );
        let text = visible_text(&line);
        assert!(text.ends_with("AC 48% 3d16h"), "{text}");
        assert!(!text.contains("AC   48%"), "{text}");
    }

    #[test]
    fn compact_left_drops_a_zero_unit() {
        assert_eq!(compact_left(17_100, false), "4h45m");
        assert_eq!(compact_left(2_100, false), "35m");
        assert_eq!(compact_left(316_800, true), "3d16h");
        assert_eq!(compact_left(57_600, true), "16h");
        assert_eq!(compact_left(-5, false), "0m");
    }

    #[test]
    fn colours_follow_the_load() {
        fn line(percent: f64) -> String {
            let mut config = StatuslineConfig::current_line();
            config.left.clear();
            config.right = vec![config.right[1].clone()];
            let mut facts = facts();
            facts.five_hour = Some(Limit2 {
                percent,
                resets_at: None,
            });
            rendered(&config, &payload(), &facts, None, &offset())
        }
        assert!(line(49.0).contains("\x1b[32m"), "{}", line(49.0));
        assert!(line(50.0).contains("\x1b[33m"), "{}", line(50.0));
        assert!(line(80.0).contains("\x1b[31m"), "{}", line(80.0));
    }

    #[test]
    fn a_named_colour_serialises_as_its_hex_and_ansi_writes_its_sgr() {
        let mut config = StatuslineConfig::current_line();
        config.left.clear();
        config.right = vec![config.right[1].clone()];
        let mut facts = facts();
        facts.five_hour = Some(Limit2 {
            percent: 49.0,
            resets_at: None,
        });

        let (spans, _) = render_spans(&config, &payload(), &facts, None, &offset());
        // The value "49" and its suffix "%" are two spans; the suffix keeps the
        // value's foreground because the rule paints no background, so `ansi`
        // writes the same run as before the split.
        let span = spans.first().expect("the 5 h percent's value span");
        assert_eq!(span.text, "49");
        assert_eq!(span.fg, Some(Color::Named(GREEN)));
        assert_eq!(
            serde_json::to_value(span).expect("a serialised span")["fg"],
            serde_json::json!("#00cd00")
        );
        assert_eq!(
            ansi(&spans),
            "\x1b[32m49%\x1b[0m",
            "ansi keeps the named colour's SGR bytes"
        );
    }

    #[test]
    fn ansi_of_spans_equals_the_old_render() {
        // The four layout tests of step B, with their expected strings.
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(120),
            &offset(),
        );
        assert_eq!(
            visible_text(&line),
            format!(
                "sam@atlas:{} Fable 5.2 high{}AC 12% 4h45m  48% 3d16h",
                crate::test_support::tilde("projects/seatswap/crates/app/ui"),
                " ".repeat(35)
            )
        );
        assert_eq!(visible_width(&line), 116);

        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(80),
            &offset(),
        );
        assert!(visible_text(&line).ends_with("AC 12%  48%"), "{line}");
        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(74),
            &offset(),
        );
        assert!(visible_text(&line).ends_with("AC 48%"), "{line}");

        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            None,
            &offset(),
        );
        assert_eq!(
            visible_text(&line),
            format!(
                "sam@atlas:{} Fable 5.2 high  AC 12% 4h45m  48% 3d16h",
                crate::test_support::tilde("projects/seatswap/crates/app/ui")
            )
        );

        let line = rendered(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(20),
            &offset(),
        );
        let text = visible_text(&line);
        assert!(text.starts_with("sam@atlas:"), "{text}");
        assert!(text.contains("AC 48%"), "{text}");
    }

    #[test]
    fn an_empty_config_is_an_empty_line() {
        let config = StatuslineConfig {
            left: Vec::new(),
            right: Vec::new(),
            margin: 4,
        };
        for width in [None, Some(80)] {
            let (spans, dropped) = render_spans(&config, &payload(), &facts(), width, &offset());
            assert!(spans.is_empty(), "no segment has a value");
            assert!(dropped.is_empty(), "nothing was dropped");
            assert_eq!(ansi(&spans), "");
        }
    }

    #[test]
    fn an_empty_zone_needs_no_gap() {
        // "Limits only" leaves the left zone empty; its right zone prints 23
        // columns and its margin is 4, so 27 columns fit it exactly. The old
        // rule reserved two more for the gap and dropped both reset times.
        let limits = StatuslineConfig::limits_only();
        let (spans, dropped) = render_spans(&limits, &payload(), &facts(), Some(27), &offset());
        assert!(dropped.is_empty(), "nothing is dropped while the line fits");
        assert_eq!(visible_text(&ansi(&spans)), "AC 12% 4h45m  48% 3d16h");

        let (spans, dropped) = render_spans(&limits, &payload(), &facts(), Some(26), &offset());
        assert_eq!(dropped, vec![1], "one column less drops the first segments");
        let text = visible_text(&ansi(&spans));
        assert!(text.ends_with("AC 12%  48%"), "{text}");
        assert_eq!(
            visible_width(&text),
            22,
            "the right zone is flush with the margin"
        );

        // The same with an empty right zone and the same segments on the left.
        let mut mirrored = StatuslineConfig::limits_only();
        mirrored.left = mirrored.right;
        mirrored.right = Vec::new();
        let (spans, dropped) = render_spans(&mirrored, &payload(), &facts(), Some(27), &offset());
        assert!(dropped.is_empty(), "nothing is dropped while the line fits");
        assert_eq!(visible_text(&ansi(&spans)), "AC 12% 4h45m  48% 3d16h");

        let (spans, dropped) = render_spans(&mirrored, &payload(), &facts(), Some(26), &offset());
        assert_eq!(dropped, vec![1], "one column less drops the first segments");
        assert_eq!(visible_text(&ansi(&spans)), "AC 12%  48%");
    }

    #[test]
    fn an_empty_text_prints_nothing() {
        let mut text = segment(SegmentKind::Text);
        text.text = Some(String::new());
        text.prefix = " | ".to_string();
        text.suffix = "!".to_string();
        let config = StatuslineConfig {
            left: vec![text],
            right: Vec::new(),
            margin: 0,
        };

        let (spans, dropped) = render_spans(&config, &payload(), &facts(), None, &offset());
        assert_eq!(
            ansi(&spans),
            "",
            "an empty text prints no prefix and no suffix"
        );
        assert!(dropped.is_empty(), "nothing was dropped");
    }

    #[test]
    fn the_account_background_covers_the_letters_only() {
        let mut config = StatuslineConfig::current_line();
        config.left = Vec::new();
        // The account segment: the value "AC" and the suffix " ".
        config.right = vec![config.right[0].clone()];

        let (spans, dropped) = render_spans(&config, &payload(), &facts(), None, &offset());
        assert!(dropped.is_empty(), "nothing was dropped");
        assert_eq!(visible_text(&ansi(&spans)), "AC ");
        assert_eq!(
            spans.len(),
            2,
            "an empty prefix makes no span: the value and the suffix"
        );
        assert_eq!(spans[0].text, "AC");
        assert!(
            spans[0].fg.is_some() && spans[0].bg.is_some(),
            "the letters carry the account colour"
        );
        assert!(
            spans[1].fg.is_none() && spans[1].bg.is_none(),
            "the suffix's space is plain"
        );

        let line = ansi(&spans);
        assert!(
            line.contains(&format!("AC{RESET} ")),
            "the background reset comes right after the letters: {line:?}"
        );
        assert!(
            !line.contains(&format!("AC {RESET}")),
            "the suffix's space carries no background: {line:?}"
        );
    }

    fn segment(kind: SegmentKind) -> Segment {
        Segment {
            kind,
            color: ColorRule::None,
            prefix: String::new(),
            suffix: String::new(),
            drop: None,
            time_format: None,
            text: None,
        }
    }

    #[test]
    fn the_optional_segments_paths_are_one_table() {
        assert_eq!(
            field_paths(SegmentKind::ContextUsed),
            &["context_window.used_percentage"][..]
        );
        assert_eq!(
            field_paths(SegmentKind::SessionCost),
            &["cost.total_cost_usd"][..]
        );
        assert_eq!(
            field_paths(SegmentKind::LinesChanged),
            &["cost.total_lines_added", "cost.total_lines_removed"][..]
        );
        assert!(field_paths(SegmentKind::Account).is_empty());
        assert_eq!(
            serde_json::to_value(all_field_paths()).expect("the table as JSON"),
            serde_json::json!([
                {"kind": "contextUsed", "paths": ["context_window.used_percentage"]},
                {"kind": "sessionCost", "paths": ["cost.total_cost_usd"]},
                {"kind": "linesChanged", "paths": ["cost.total_lines_added", "cost.total_lines_removed"]}
            ])
        );
    }

    #[test]
    fn the_optional_segments_read_their_paths() {
        let payload = super::super::parse(
            br#"{"context_window":{"used_percentage":41},"cost":{"total_cost_usd":3.2,"total_lines_added":120,"total_lines_removed":8}}"#,
        );
        let mut cost = segment(SegmentKind::SessionCost);
        cost.prefix = " ".to_string();
        let mut lines = segment(SegmentKind::LinesChanged);
        lines.prefix = " ".to_string();
        let config = StatuslineConfig {
            left: vec![segment(SegmentKind::ContextUsed), cost, lines],
            right: Vec::new(),
            margin: 0,
        };

        assert_eq!(
            rendered(&config, &payload, &facts(), None, &offset()),
            "41% $3.20 +120 -8"
        );
    }

    #[test]
    fn an_absent_path_prints_nothing() {
        let payload = super::super::parse(super::super::README_PAYLOAD.as_bytes());
        let mut text = segment(SegmentKind::Text);
        text.text = Some("x".to_string());
        let mut context = segment(SegmentKind::ContextUsed);
        context.prefix = " ctx ".to_string();
        let config = StatuslineConfig {
            left: vec![text, context],
            right: Vec::new(),
            margin: 0,
        };

        let line = rendered(&config, &payload, &facts(), None, &offset());
        assert_eq!(visible_text(&line), "x");
        assert!(!line.contains("ctx"), "{line}");
    }

    #[test]
    fn a_null_field_prints_nothing_and_a_whole_cost_is_money() {
        let payload = super::super::parse(
            br#"{"context_window":{"used_percentage":null},"cost":{"total_cost_usd":0}}"#,
        );
        let mut context = segment(SegmentKind::ContextUsed);
        context.prefix = " ctx ".to_string();
        let config = StatuslineConfig {
            left: vec![context],
            right: vec![segment(SegmentKind::SessionCost)],
            margin: 0,
        };

        let line = rendered(&config, &payload, &facts(), None, &offset());
        assert_eq!(visible_text(&line), "$0.00");
        assert!(!line.contains("ctx"), "{line}");
    }

    #[test]
    fn the_preview_says_what_was_dropped() {
        // A home folder that is not the payload's prefix shows the folder in
        // full, so 80 columns need the second drop number as well.
        let mut live_facts = facts();
        live_facts.home = "/home/samuel".to_string();
        let preview = super::super::preview_lines(
            &StatuslineConfig::current_line(),
            &payload(),
            &live_facts,
            &[120, 100, 80],
            super::super::Scenario::Live,
            &offset(),
        )
        .expect("a preview at three widths");

        assert_eq!(preview.len(), 3);
        assert_eq!(preview[0].note, "everything fits");
        assert_eq!(preview[1].note, "everything fits");
        assert_eq!(
            preview[2].note,
            "5 h reset, week reset and 5 h percent are dropped"
        );
        assert_eq!(preview[2].dropped, vec![1, 2]);
    }

    #[test]
    fn the_note_names_only_the_segments_the_fitting_dropped() {
        // The 5 h percent has a value but its reset has none, so the reset is
        // not a piece the fitting can drop. Only the 5 h percent and the week
        // reset were on the line and removed.
        let mut live_facts = facts();
        live_facts.five_hour = Some(Limit2 {
            percent: 12.0,
            resets_at: None,
        });
        let preview = super::super::preview_lines(
            &StatuslineConfig::current_line(),
            &payload(),
            &live_facts,
            &[60],
            super::super::Scenario::Live,
            &offset(),
        )
        .expect("a preview at 60 columns");

        assert_eq!(
            preview[0].note, "week reset and 5 h percent are dropped",
            "the absent 5 h reset is not named"
        );
        assert!(
            !preview[0].note.contains("5 h reset"),
            "{}",
            preview[0].note
        );
    }

    #[test]
    fn presets() {
        let presets = StatuslineConfig::presets();
        assert_eq!(presets.len(), 3);
        assert_eq!(presets[0].0, "My current line");
        assert_eq!(presets[0].1, StatuslineConfig::current_line());
        assert_eq!(presets[1].0, "Limits only");
        assert_eq!(presets[2].0, "Everything");

        let kinds: Vec<SegmentKind> = presets[2]
            .1
            .left
            .iter()
            .chain(presets[2].1.right.iter())
            .map(|segment| segment.kind)
            .collect();
        assert_eq!(kinds.len(), 19, "every kind once");
        for (index, kind) in kinds.iter().enumerate() {
            assert!(!kinds[..index].contains(kind), "{kind:?} appears once only");
        }
    }

    #[test]
    fn a_preview_width_is_positive_and_not_empty() {
        let config = StatuslineConfig::current_line();
        let empty = super::super::preview_lines(
            &config,
            &payload(),
            &facts(),
            &[],
            super::super::Scenario::Live,
            &offset(),
        );
        assert_eq!(
            empty.err(),
            Some("the preview needs at least one width".to_string())
        );
        let zero = super::super::preview_lines(
            &config,
            &payload(),
            &facts(),
            &[120, 0],
            super::super::Scenario::Live,
            &offset(),
        );
        assert_eq!(
            zero.err(),
            Some("a preview width must be at least 1 column; 0 cannot show a line".to_string())
        );
    }

    /// `visible_width` is the number of columns the line takes in the
    /// terminal, but it counts characters: a wide character takes two columns
    /// and a combining mark takes none. A folder name with such characters
    /// makes the line wider than the width it was fitted to. This test states
    /// the column count and fails today.
    #[test]
    fn visible_width_counts_columns_and_not_characters() {
        assert_eq!(
            visible_width("日本語"),
            6,
            "three wide characters are six columns"
        );
        assert_eq!(
            visible_width("e\u{301}"),
            1,
            "an e with a combining acute is one column"
        );
    }

    /// The renderer drops a prefix's leading spaces when the text before it
    /// ends in a space. The plan gives that rule for a segment without a
    /// value only ("no session limit → FiveHourPercent and FiveHourReset
    /// vanish"); here every segment has a value, so the week percent's prefix
    /// of two spaces belongs to the line. This test states the plan's rule
    /// and fails today.
    #[test]
    fn a_prefix_is_printed_as_it_is_when_the_segment_has_a_value() {
        let mut config = StatuslineConfig::current_line();
        // The account (its suffix is a space) directly before the week
        // percent (its prefix is two spaces).
        config.right = vec![config.right[0].clone(), config.right[3].clone()];
        let line = rendered(&config, &payload(), &facts(), None, &offset());
        assert!(
            visible_text(&line).ends_with("AC   48%"),
            "the prefix of the week percent belongs to the line: {line}"
        );
    }

    /// The repository is found with core's walk from the session's folder: a
    /// folder two levels below the root still shows the branch, a worktree (a
    /// `.git` file naming its git dir) shows the branch in that git dir, and a
    /// detached HEAD shows none.
    #[test]
    fn git_branch_reads_head() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let repo = home.join("projects/atlas");
        let git = repo.join(".git");
        std::fs::create_dir_all(&git).expect("the .git folder");
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        let cwd = repo.join("engine/tests");
        std::fs::create_dir_all(&cwd).expect("a folder two levels below the root");
        assert_eq!(git_branch(&cwd, &home), Some("main".to_string()));

        // A worktree: `.git` is a file naming another folder, and that folder's
        // HEAD names the branch.
        let worktree = home.join("projects/atlas-w1");
        std::fs::create_dir_all(&worktree).expect("the worktree folder");
        let gitdir = tmp.path().join("git/worktrees/w1");
        std::fs::create_dir_all(&gitdir).expect("the worktree's git dir");
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/atlas-w1\n").expect("HEAD");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .expect("the .git file");
        assert_eq!(git_branch(&worktree, &home), Some("atlas-w1".to_string()));

        std::fs::write(
            git.join("HEAD"),
            "9c2f3b7d5e1a4c6b8d0f2e4a6c8b0d2f4e6a8c0b\n",
        )
        .expect("a detached HEAD");
        assert_eq!(git_branch(&repo, &home), None);
    }
}
