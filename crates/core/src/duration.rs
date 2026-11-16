//! Every duration the program writes, in one spelling (review item 42):
//! `12s`, `12min`, `2h 31min`, `3d 4h`.
//!
//! No space between a number and its unit, one space between the two units, at
//! most two units, never a leading zero. Below a minute the seconds are shown,
//! below an hour the whole minutes, below a day the whole hours and minutes;
//! from a day on the whole days and hours, without the minutes (documented
//! rule). The caller rounds: this function floors the seconds it is given.

/// `0s`, `59s`, `1min`, `2h 31min`, `1d`, `3d 4h`.
pub fn duration_text(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3_600 {
        return format!("{}min", seconds / 60);
    }
    if seconds < 86_400 {
        let hours = seconds / 3_600;
        let minutes = seconds % 3_600 / 60;
        if minutes == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h {minutes}min");
    }
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    if hours == 0 {
        return format!("{days}d");
    }
    format!("{days}d {hours}h")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_written_without_a_space() {
        assert_eq!(duration_text(0), "0s");
        assert_eq!(duration_text(59), "59s");
        assert_eq!(duration_text(60), "1min");
        assert_eq!(duration_text(3_599), "59min");
        assert_eq!(duration_text(3_600), "1h");
        assert_eq!(duration_text(2 * 3_600 + 31 * 60), "2h 31min");
        assert_eq!(duration_text(23 * 3_600 + 59 * 60), "23h 59min");
        assert_eq!(duration_text(86_400), "1d");
        assert_eq!(
            duration_text(3 * 86_400 + 4 * 3_600 + 59 * 60),
            "3d 4h",
            "the minutes of a day or more are not shown"
        );
    }

    #[test]
    fn a_unit_with_a_zero_count_is_left_out() {
        assert_eq!(duration_text(2 * 3_600), "2h");
        assert_eq!(duration_text(2 * 86_400), "2d");
        assert_eq!(duration_text(86_400 + 3 * 3_600), "1d 3h");
    }
}
