//! Column formatting (`meld/ui/cellrenderers.py`).

use std::time::{SystemTime, UNIX_EPOCH};

/// `CellRendererByteSize`: decimal units, one decimal place above bytes.
pub fn byte_size(size: u64) -> String {
    const SUFFIXES: [&str; 9] = ["B", "kB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value > 1000.0 && unit < SUFFIXES.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} {}", SUFFIXES[0])
    } else {
        format!("{value:.1} {}", SUFFIXES[unit])
    }
}

/// `CellRendererFileMode`: `rwxr-xr-x`.
pub fn file_mode(mode: u32) -> String {
    let mut out = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let group = (mode >> shift) & 7;
        out.push(if group & 4 != 0 { 'r' } else { '-' });
        out.push(if group & 2 != 0 { 'w' } else { '-' });
        out.push(if group & 1 != 0 { 'x' } else { '-' });
    }
    out
}

/// Broken-down local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1-12
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday
    pub weekday: u32,
}

impl LocalTime {
    pub fn from_system_time(time: SystemTime) -> Option<LocalTime> {
        let secs = match time.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).ok()?,
            Err(e) => -i64::try_from(e.duration().as_secs()).ok()?,
        };
        let t = secs as libc::time_t;
        // SAFETY: localtime_r only writes into the provided struct.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&t, &mut tm) };
        if result.is_null() {
            return None;
        }
        Some(LocalTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            weekday: tm.tm_wday as u32,
        })
    }

    /// `CellRendererDate`: `"%a %d %b %Y %H:%M:%S"` in the C locale.
    pub fn format(&self) -> String {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{} {:02} {} {} {:02}:{:02}:{:02}",
            DAYS[self.weekday as usize % 7],
            self.day,
            MONTHS[(self.month as usize + 11) % 12],
            self.year,
            self.hour,
            self.minute,
            self.second
        )
    }

    /// `CellRendererISODate`: ISO 8601 to the second.
    pub fn format_iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_like_meld() {
        assert_eq!(byte_size(0), "0 B");
        assert_eq!(byte_size(1000), "1000 B");
        assert_eq!(byte_size(1001), "1.0 kB");
        assert_eq!(byte_size(4096), "4.1 kB");
        assert_eq!(byte_size(1_500_000), "1.5 MB");
        assert_eq!(byte_size(3_000_000_000), "3.0 GB");
    }

    #[test]
    fn modes() {
        assert_eq!(file_mode(0o100644), "rw-r--r--");
        assert_eq!(file_mode(0o40755), "rwxr-xr-x");
        assert_eq!(file_mode(0), "---------");
    }

    #[test]
    fn dates() {
        let t = LocalTime {
            year: 2024,
            month: 3,
            day: 5,
            hour: 7,
            minute: 8,
            second: 9,
            weekday: 2,
        };
        assert_eq!(t.format(), "Tue 05 Mar 2024 07:08:09");
        assert_eq!(t.format_iso(), "2024-03-05T07:08:09");
        assert!(LocalTime::from_system_time(SystemTime::now()).is_some());
    }
}
