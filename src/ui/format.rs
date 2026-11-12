//! Human-friendly number and date formatting.

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// "1 item", "2,048 items".
pub fn items(n: u64) -> String {
    plural(n, "item", "items")
}

pub fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", count(n))
    }
}

/// Seconds as "4 s", "2 min", "1 h 5 min".
pub fn eta(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s.div_ceil(60))
    } else {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[derive(Clone, Copy)]
struct Local {
    year: i32,
    month: usize,
    day: i32,
    yday: i32,
    wday: usize,
    hour: i32,
    min: i32,
}

fn breakdown(secs: i64, utc: bool) -> Local {
    let tm = tm(secs, utc);
    Local {
        year: tm.tm_year + 1900,
        month: tm.tm_mon.clamp(0, 11) as usize,
        day: tm.tm_mday,
        yday: tm.tm_yday,
        wday: tm.tm_wday.clamp(0, 6) as usize,
        hour: tm.tm_hour,
        min: tm.tm_min,
    }
}

fn tm(secs: i64, utc: bool) -> libc::tm {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        if utc {
            libc::gmtime_r(&t, &mut tm);
        } else {
            libc::localtime_r(&t, &mut tm);
        }
    }
    tm
}

/// Formats dates relative to a fixed "now", so a whole listing agrees.
#[derive(Clone, Copy)]
pub struct DateFormatter {
    now_secs: i64,
    now: Local,
    utc: bool,
}

impl DateFormatter {
    pub fn new(utc: bool) -> Self {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self {
            now_secs,
            now: breakdown(now_secs, utc),
            utc,
        }
    }

    /// "Today 14:05", "Yesterday 09:12", "Mon 18:30", "Mar 3", "Mar 3, 2023".
    pub fn short(&self, secs: i64) -> String {
        if secs <= 0 {
            return String::new();
        }
        let t = breakdown(secs, self.utc);
        let age = self.now_secs - secs;
        if t.year == self.now.year && t.yday == self.now.yday {
            format!("Today {:02}:{:02}", t.hour, t.min)
        } else if (0..2 * 86400).contains(&age) && self.days_between(&t) == 1 {
            format!("Yesterday {:02}:{:02}", t.hour, t.min)
        } else if (0..6 * 86400).contains(&age) {
            format!("{} {:02}:{:02}", DAYS[t.wday], t.hour, t.min)
        } else if t.year == self.now.year {
            format!("{} {}", MONTHS[t.month], t.day)
        } else {
            format!("{} {}, {}", MONTHS[t.month], t.day, t.year)
        }
    }

    fn days_between(&self, t: &Local) -> i32 {
        if t.year == self.now.year {
            self.now.yday - t.yday
        } else {
            // Across New Year: Dec 31 → Jan 1.
            self.now.yday + 1 + (if t.month == 11 && t.day == 31 { 0 } else { 99 })
        }
    }
}

/// "rwxr-xr-x" for the low nine mode bits.
pub fn permissions(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// "4.2 MB/s".
pub fn rate(bytes_per_sec: f64) -> String {
    format!("{}/s", bytes(bytes_per_sec.max(0.0) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.50 KB");
        assert_eq!(bytes(200 * 1024 * 1024), "200 MB");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(items(1), "1 item");
        assert_eq!(items(2048), "2,048 items");
        assert_eq!(permissions(0o755), "rwxr-xr-x");
        assert_eq!(eta(30.0), "30 s");
    }

    #[test]
    fn relative_dates() {
        let f = DateFormatter::new(false);
        assert!(f.short(f.now_secs - 10).starts_with("Today"));
        assert_eq!(f.short(0), "");
        assert!(f.short(f.now_secs - 400 * 86400).contains(", "));
    }
}
