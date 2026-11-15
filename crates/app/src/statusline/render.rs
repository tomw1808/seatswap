//! The status line itself: one layout of segments, coloured, fitted to a
//! terminal width (spec §5.1, §5.4; step B, Task 2).
//!
//! Everything here is pure: the numbers come in as [`Facts`], the layout from
//! the stored [`StatuslineConfig`], and the time zone is a parameter so the
//! tests can fix an offset. No file is read except `.git/HEAD` for the branch.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};

use seatswap_core::store::{ColorRule, Segment, SegmentKind, StatuslineConfig, TimeFormat};

use super::Payload;
use crate::format::{
    compact_left, format_reset_at, format_reset_clock, integer_percent, letter_color, parse_color,
    with_tilde,
};

/// The green / yellow / red (and bright) SGR prefixes of the colour rules.
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";
const BRIGHT_YELLOW: &str = "\x1b[93m";
const BRIGHT_RED: &str = "\x1b[91m";
const RESET: &str = "\x1b[0m";

/// What the line shows besides the payload's model, effort and folder.
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
pub(crate) struct AccountFacts {
    pub abbr: String,
    pub color: String,
    pub plan: Option<String>,
}

/// One limit: a percent and its reset, when the source has one.
pub(crate) struct Limit2 {
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// A segment that has a value, ready to colour and place. `prefix` is kept
/// apart from `text` because a prefix's leading spaces are dropped when the
/// text before it ends in a space and a segment between them was left out: a
/// missing segment leaves no gap.
struct Piece {
    prefix: String,
    text: String,
    suffix: String,
    sgr: Option<String>,
    drop: Option<u8>,
    /// At least one segment between the previous piece and this one in the
    /// configuration has no value. Together with a segment dropped by the
    /// fitting, this is what makes a prefix's leading spaces go away.
    missing_before: bool,
}

/// The line with ANSI colours. `width = None`: the right part follows the left
/// after two spaces and nothing is dropped. With a width the right part is
/// flush with `width - margin`, and segments are dropped by their `drop` number
/// until the line fits; when nothing is left to drop it is printed anyway.
pub(crate) fn render<Tz: TimeZone>(
    config: &StatuslineConfig,
    payload: &Payload,
    facts: &Facts,
    width: Option<u16>,
    tz: &Tz,
) -> String
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

    let (left_text, right_text) = compose(&left, &right, level);
    let left_width = visible_width(&left_text);
    let right_width = visible_width(&right_text);
    if right_text.is_empty() {
        return left_text;
    }
    if left_text.is_empty() {
        return match width {
            Some(width) => {
                let target = usize::from(width).saturating_sub(usize::from(config.margin));
                format!(
                    "{}{right_text}",
                    " ".repeat(target.saturating_sub(right_width))
                )
            }
            None => right_text,
        };
    }
    let gap = match width {
        Some(width) => {
            let target = usize::from(width).saturating_sub(usize::from(config.margin));
            target.saturating_sub(left_width + right_width).max(2)
        }
        None => 2,
    };
    format!("{left_text}{}{right_text}", " ".repeat(gap))
}

/// Whether the line built from the segments above `level` keeps the margin and
/// the two spaces between the zones.
fn fits(left: &[Piece], right: &[Piece], level: Option<u8>, margin: u16, width: u16) -> bool {
    let (left_text, right_text) = compose(left, right, level);
    visible_width(&left_text) + 2 + visible_width(&right_text) + usize::from(margin)
        <= usize::from(width)
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

/// The branch of the repository at `cwd`, from `.git/HEAD`; a detached HEAD
/// (a commit hash) has none. Nothing is executed and no worktree file is read.
pub(crate) fn git_branch(cwd: &Path) -> Option<String> {
    let head = std::fs::read_to_string(cwd.join(".git").join("HEAD")).ok()?;
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
            prefix: segment.prefix.clone(),
            text,
            suffix: segment.suffix.clone(),
            sgr: color(segment, payload, facts),
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
        SegmentKind::Text => segment.text.clone(),
        SegmentKind::Separator => Some("|".to_string()),
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

/// The SGR prefix of a segment's colour rule, if it has one.
fn color(segment: &Segment, payload: &Payload, facts: &Facts) -> Option<String> {
    match &segment.color {
        ColorRule::None => None,
        ColorRule::Fixed { color } => {
            let [red, green, blue] = parse_color(color)?;
            Some(format!("\x1b[38;2;{red};{green};{blue}m"))
        }
        ColorRule::ByLoad { warn, bad } => {
            let percent = load_percent(segment.kind, facts)?;
            if percent >= f64::from(*bad) {
                Some(RED.to_string())
            } else if percent >= f64::from(*warn) {
                Some(YELLOW.to_string())
            } else {
                Some(GREEN.to_string())
            }
        }
        ColorRule::ByModelFamily => {
            let id = payload
                .model
                .as_ref()
                .and_then(|model| model.id.as_deref())?
                .to_ascii_lowercase();
            if id.contains("opus") {
                Some(MAGENTA.to_string())
            } else if id.contains("sonnet") {
                Some(CYAN.to_string())
            } else if id.contains("haiku") {
                Some(GREEN.to_string())
            } else {
                Some(YELLOW.to_string())
            }
        }
        ColorRule::ByEffort => {
            let level = payload
                .effort
                .as_ref()
                .and_then(|effort| effort.level.as_deref())?;
            match level {
                "low" => Some(GREEN.to_string()),
                "medium" => Some(YELLOW.to_string()),
                "high" => Some(BRIGHT_YELLOW.to_string()),
                "xhigh" => Some(RED.to_string()),
                "max" => Some(BRIGHT_RED.to_string()),
                _ => None,
            }
        }
        ColorRule::Account => {
            let [red, green, blue] = parse_color(&facts.account.as_ref()?.color)?;
            let [text_red, text_green, text_blue] = letter_color([red, green, blue]);
            Some(format!(
                "\x1b[48;2;{red};{green};{blue}m\x1b[38;2;{text_red};{text_green};{text_blue}m"
            ))
        }
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

/// The two zones as they print with the drop `level`: every segment whose drop
/// number is above `level` (or has none) stays. A segment's prefix loses its
/// leading spaces only when the text before it ends in a space and at least one
/// segment between them was left out (no value, or dropped by the fitting), so
/// a gap leaves no leading spaces while two segments adjacent in the
/// configuration print their suffix and prefix as written.
fn compose(left: &[Piece], right: &[Piece], level: Option<u8>) -> (String, String) {
    (compose_zone(left, level), compose_zone(right, level))
}

fn compose_zone(pieces: &[Piece], level: Option<u8>) -> String {
    let mut out = String::new();
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
        match &piece.sgr {
            Some(sgr) => {
                out.push_str(sgr);
                out.push_str(&text);
                out.push_str(RESET);
            }
            None => out.push_str(&text),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::FixedOffset;
    use seatswap_core::store::StatuslineConfig;

    /// The status line payload of `specs/plan/README.md` §5, with the folder the
    /// Task 2 tests name.
    const PAYLOAD: &str = r#"{"workspace":{"current_dir":"/home/sam/projects/seatswap/crates/app/ui"},"model":{"id":"claude-fable-5-2","display_name":"Fable 5.2"},"effort":{"level":"high"},"rate_limits":{"five_hour":{"used_percentage":12,"resets_at":1794490200},"seven_day":{"used_percentage":48,"resets_at":1794790800}}}"#;

    /// 4 h 45 min before the 5 h reset, 3 d 16 h before the weekly one.
    const NOW: i64 = 1_794_473_100;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(seconds, 0).expect("a valid test timestamp")
    }

    fn payload() -> Payload {
        super::super::parse(PAYLOAD.as_bytes())
    }

    fn facts() -> Facts {
        Facts {
            user: "sam".to_string(),
            host: "atlas".to_string(),
            home: "/home/sam".to_string(),
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

    #[test]
    fn the_current_line_at_120_columns() {
        let line = render(
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
                "sam@atlas:~/projects/seatswap/crates/app/ui Fable 5.2 high{}AC 12% 4h45m  48% 3d16h",
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
        let line = render(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            Some(80),
            &offset(),
        );
        assert!(visible_text(&line).ends_with("AC 12%  48%"), "{line}");

        // The next drop number removes the 5 h percent too, and the week
        // percent's separator leaves no gap after it.
        let line = render(
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
        let line = render(
            &StatuslineConfig::current_line(),
            &payload(),
            &facts(),
            None,
            &offset(),
        );
        assert_eq!(
            visible_text(&line),
            "sam@atlas:~/projects/seatswap/crates/app/ui Fable 5.2 high  AC 12% 4h45m  48% 3d16h"
        );
    }

    #[test]
    fn narrower_than_the_left_part_still_prints() {
        let line = render(
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
        let line = render(
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
            render(&config, &payload(), &facts, None, &offset())
        }
        assert!(line(49.0).contains("\x1b[32m"), "{}", line(49.0));
        assert!(line(50.0).contains("\x1b[33m"), "{}", line(50.0));
        assert!(line(80.0).contains("\x1b[31m"), "{}", line(80.0));
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
        let line = render(&config, &payload(), &facts(), None, &offset());
        assert!(
            visible_text(&line).ends_with("AC   48%"),
            "the prefix of the week percent belongs to the line: {line}"
        );
    }

    #[test]
    fn git_branch_reads_head() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let git = tmp.path().join(".git");
        std::fs::create_dir_all(&git).expect("the .git folder");
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        assert_eq!(git_branch(tmp.path()), Some("main".to_string()));

        std::fs::write(
            git.join("HEAD"),
            "9c2f3b7d5e1a4c6b8d0f2e4a6c8b0d2f4e6a8c0b\n",
        )
        .expect("a detached HEAD");
        assert_eq!(git_branch(tmp.path()), None);
    }
}
