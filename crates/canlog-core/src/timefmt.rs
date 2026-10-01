//! Minimal UTC date formatting (no chrono dependency).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub ms: u32,
    /// 0 = Sunday
    pub weekday: u32,
}

/// Convert milliseconds since the Unix epoch to a UTC civil date.
pub fn civil(utc_ms: i64) -> Civil {
    let days = utc_ms.div_euclid(86_400_000);
    let rem = utc_ms.rem_euclid(86_400_000);
    // Howard Hinnant's days_from_civil inverse
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    Civil {
        year,
        month: m,
        day: d,
        hour: (rem / 3_600_000) as u32,
        min: (rem / 60_000 % 60) as u32,
        sec: (rem / 1000 % 60) as u32,
        ms: (rem % 1000) as u32,
        weekday: ((days + 4).rem_euclid(7)) as u32,
    }
}

const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MO: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `2026-09-30_1412` style, for file names.
pub fn file_stamp(utc_ms: i64) -> String {
    let c = civil(utc_ms);
    format!("{:04}-{:02}-{:02}_{:02}{:02}{:02}", c.year, c.month, c.day, c.hour, c.min, c.sec)
}

/// ISO 8601 `2026-09-30T14:12:05.123Z`.
pub fn iso(utc_ms: i64) -> String {
    let c = civil(utc_ms);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", c.year, c.month, c.day, c.hour, c.min, c.sec, c.ms)
}

/// Vector ASC header date: `Wed Sep 30 02:12:05.123 pm 2026`.
pub fn asc_date(utc_ms: i64) -> String {
    let c = civil(utc_ms);
    let (h12, ampm) = match c.hour {
        0 => (12, "am"),
        1..=11 => (c.hour, "am"),
        12 => (12, "pm"),
        _ => (c.hour - 12, "pm"),
    };
    format!(
        "{} {} {:02} {:02}:{:02}:{:02}.{:03} {} {}",
        WD[c.weekday as usize],
        MO[(c.month - 1) as usize],
        c.day,
        h12,
        c.min,
        c.sec,
        c.ms,
        ampm,
        c.year
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        // 2026-09-30T12:12:05.123Z is a Wednesday
        let t = 1_790_770_325_123;
        assert_eq!(iso(t), "2026-09-30T12:12:05.123Z");
        assert_eq!(asc_date(t), "Wed Sep 30 12:12:05.123 pm 2026");
        assert_eq!(file_stamp(t), "2026-09-30_121205");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }
}
