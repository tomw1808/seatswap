//! The terminal width of the status line (spec §5.1, §5.2): the terminal of the
//! nearest ancestor process that has one (Linux), else `$COLUMNS`, else none.
//!
//! Claude Code starts the status line command on a pipe, so the command has no
//! terminal of its own; walking up the parent processes reaches the shell that
//! has one. Opening the terminal without `unsafe` is what `rustix` is for.
//! A terminal that cannot be opened is skipped, not an error: the next ancestor
//! may still have one that opens.

/// The width of the terminal of the nearest ancestor process that has one,
/// else `$COLUMNS`, else `None`. `None` means the right part follows the left
/// after two spaces.
pub(crate) fn terminal_width() -> Option<u16> {
    #[cfg(target_os = "linux")]
    if let Some(width) = terminal_width_of_ancestors() {
        return Some(width);
    }
    columns()
}

/// `$COLUMNS`: Claude Code sets it for the command (checked 15 November 2026:
/// 120 on a 120-column terminal), and Git Bash passes it through on Windows.
fn columns() -> Option<u16> {
    let value = std::env::var("COLUMNS").ok()?;
    parse_columns(Some(value.as_str()))
}

/// A whole, positive number of columns. `COLUMNS` that is empty, not a number
/// or zero is ignored, never clamped to a guess.
fn parse_columns(value: Option<&str>) -> Option<u16> {
    let columns: u16 = value?.trim().parse().ok()?;
    if columns == 0 {
        None
    } else {
        Some(columns)
    }
}

/// Walk from this process to the first ancestor whose `tty_nr` names a
/// pseudo-terminal that can be opened, and read that terminal's size.
#[cfg(target_os = "linux")]
fn terminal_width_of_ancestors() -> Option<u16> {
    // `ppid` cannot form a cycle; the bound only keeps a damaged or unreadable
    // /proc from spinning.
    const MAX_DEPTH: usize = 64;
    let mut pid = std::process::id();
    for _ in 0..MAX_DEPTH {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (parent, tty_nr) = stat_parent_and_tty(&stat)?;
        if tty_nr != 0 {
            // A device that is not a pseudo-terminal (a text console, a serial
            // line) has no /dev/pts entry; the walk continues above it.
            if let Some(width) = pts_width(tty_nr) {
                return Some(width);
            }
        }
        if parent == 0 || parent == pid {
            return None;
        }
        pid = parent;
    }
    None
}

/// The terminal size behind a `/proc/<pid>/stat` `tty_nr`: the pseudo-terminal's
/// `/dev/pts/<n>` opened read-only, then `tcgetwinsize`. `None` when the device
/// is not a pseudo-terminal, cannot be opened, or reports no columns.
#[cfg(target_os = "linux")]
fn pts_width(tty_nr: i64) -> Option<u16> {
    let number = pts_number(tty_nr)?;
    let terminal = std::fs::File::open(format!("/dev/pts/{number}")).ok()?;
    let size = rustix::termios::tcgetwinsize(&terminal).ok()?;
    if size.ws_col == 0 {
        None
    } else {
        Some(size.ws_col)
    }
}

/// The `/dev/pts/<n>` number of a `tty_nr` value, or `None` for another device.
/// The kernel packs the major and minor numbers into the field with
/// `new_encode_dev`: major in bits 8..=19, minor's low byte in 0..=7 and its
/// high bits in 20..=31.
#[cfg(target_os = "linux")]
fn pts_number(tty_nr: i64) -> Option<u32> {
    let device = u32::try_from(tty_nr).ok()?;
    let major = (device >> 8) & 0xfff;
    let minor = (device & 0xff) | ((device >> 12) & 0xfff00);
    // The pty slaves are majors 136..=143 (Unix98 ptys).
    if (136..=143).contains(&major) {
        Some(minor)
    } else {
        None
    }
}

/// The process id of the parent and `tty_nr` (field 4 and field 7) of one
/// `/proc/<pid>/stat` line. The command name in field 2 sits in parentheses,
/// may hold spaces and `)` itself, and the kernel's own closing `)` is the last
/// one in the line: splitting after it keeps every field in its place.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stat_parent_and_tty(line: &str) -> Option<(u32, i64)> {
    let close = line.rfind(')')?;
    let mut fields = line.get(close + 1..)?.split_ascii_whitespace();
    let _state = fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    let _pgrp = fields.next()?;
    let _session = fields.next()?;
    let tty_nr = fields.next()?.parse().ok()?;
    Some((parent, tty_nr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ppid_and_tty_nr_of_a_stat_line() {
        // Real layout of /proc/<pid>/stat for a process with a pty (tty_nr
        // 34816 = major 136, minor 0, /dev/pts/0).
        let line = "41822 (claude) S 41790 41822 41822 34816 41822 4194304 1234 \
                    0 0 0 0 0 0 0 0 20 0 1 0 19664719 123456789 789 18446744073709551615";
        assert_eq!(stat_parent_and_tty(line), Some((41790, 34816)));

        // The command name can hold a space.
        let line = "20312 (claude code) S 1 20312 20312 0 -1 4194304 0 0 0 0 0";
        assert_eq!(stat_parent_and_tty(line), Some((1, 0)));

        // ... and a closing parenthesis.
        let line = "7 (a)b) S 3 7 7 0 -1 4194304 0 0 0 0 0";
        assert_eq!(stat_parent_and_tty(line), Some((3, 0)));

        // A line without the command name's closing parenthesis is not a stat line.
        assert_eq!(stat_parent_and_tty("41822 claude S 41790 0 0 0 0"), None);
        // A field that is not a number is not read as one.
        assert_eq!(stat_parent_and_tty("1 (x) S nope 1 1 0 0"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_tty_nr_names_a_pseudo_terminal_only() {
        // (136 << 8) | 0 and (136 << 8) | 5: /dev/pts/0 and /dev/pts/5.
        assert_eq!(pts_number(34816), Some(0));
        assert_eq!(pts_number(34821), Some(5));
        // A virtual console (major 4), no terminal (0) and -1 are not ptys.
        assert_eq!(pts_number(1024), None);
        assert_eq!(pts_number(0), None);
        assert_eq!(pts_number(-1), None);
    }

    #[test]
    fn columns_are_a_positive_whole_number() {
        assert_eq!(parse_columns(Some("120")), Some(120));
        assert_eq!(parse_columns(Some(" 80 ")), Some(80));
        assert_eq!(parse_columns(Some("0")), None);
        assert_eq!(parse_columns(Some("abc")), None);
        assert_eq!(parse_columns(Some("")), None);
        assert_eq!(parse_columns(None), None);
    }
}
