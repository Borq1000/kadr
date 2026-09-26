//! Small formatting / OS helpers shared by views.

use kadr_i18n::{t, tf, tn};
use slint::Color;

pub fn rgb(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0xe5a50a);
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Ruler label for `t` seconds, precision matching the major tick step.
pub fn fmt_ruler(t: f64, step: f64, fps: f64) -> String {
    let total = t.max(0.0);
    let h = (total / 3600.0).floor() as u64;
    let m = ((total / 60.0).floor() as u64) % 60;
    let s = (total.floor() as u64) % 60;
    if step < 1.0 {
        let f = ((total - total.floor()) * fps).round() as u64;
        format!("{m:02}:{s:02}:{f:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Opens Explorer with the file selected.
pub fn reveal(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

pub fn open_folder(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// Opens a file with its default application.
pub fn open_file(path: &std::path::Path) {
    let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path).spawn();
}

pub fn money(v: f64) -> String {
    if v == 0.0 {
        "$0.00".into()
    } else if v < 0.01 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

pub fn money_range(lo: f64, hi: f64) -> String {
    if (hi - lo).abs() < 1e-9 { money(hi) } else { format!("{}–{}", money(lo), money(hi)) }
}

pub fn fmt_tokens(n: u64) -> String {
    let v = if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() };
    tf("ai.tokens", &[("n", &v)])
}

pub fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        tf("unit.gb", &[("v", &format!("{:.1}", b / 1e9))])
    } else if b >= 1e6 {
        tf("unit.mb", &[("v", &format!("{:.0}", b / 1e6))])
    } else {
        tf("unit.kb", &[("v", &format!("{:.0}", b / 1e3))])
    }
}

/// Local timezone offset in minutes (east positive).
fn local_offset_min() -> i64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime([u16; 8]);
        #[repr(C)]
        struct Tzi {
            bias: i32,
            standard_name: [u16; 32],
            standard_date: SystemTime,
            standard_bias: i32,
            daylight_name: [u16; 32],
            daylight_date: SystemTime,
            daylight_bias: i32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetTimeZoneInformation(tzi: *mut Tzi) -> u32;
        }
        let mut tzi: Tzi = unsafe { std::mem::zeroed() };
        // SAFETY: tzi is a correctly sized TIME_ZONE_INFORMATION.
        let r = unsafe { GetTimeZoneInformation(&mut tzi) };
        let dst = if r == 2 { tzi.daylight_bias } else { tzi.standard_bias };
        -(tzi.bias + dst) as i64
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// `HH:MM` in local time.
pub fn clock_time(ms: i64) -> String {
    let local_min = ms.div_euclid(60_000) + local_offset_min();
    let m = local_min.rem_euclid(24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// "just now", "5 min ago", "yesterday", "12.09.2026".
pub fn relative_time(ms: i64) -> String {
    let now = kadr_project::now_ms();
    let d = (now - ms).max(0) / 1000;
    if d < 60 {
        t("time.just_now")
    } else if d < 3600 {
        tn("time.min_ago", d / 60, &[])
    } else if d < 86_400 {
        tn("time.hours_ago", d / 3600, &[])
    } else if d < 2 * 86_400 {
        t("time.yesterday")
    } else if d < 30 * 86_400 {
        tn("time.days_ago", d / 86_400, &[])
    } else {
        let days = (ms / 60_000 + local_offset_min()).div_euclid(1440);
        let (y, m, dd) = civil(days);
        format!("{dd:02}.{m:02}.{y}")
    }
}

/// Days since epoch → (year, month, day) (Howard Hinnant).
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_labels() {
        assert_eq!(fmt_ruler(65.0, 5.0, 25.0), "01:05");
        assert_eq!(fmt_ruler(3725.0, 60.0, 25.0), "1:02:05");
        assert_eq!(fmt_ruler(1.4, 0.2, 25.0), "00:01:10");
        assert_eq!(money(0.00012), "$0.0001");
        assert_eq!(money_range(0.01, 0.05), "$0.01–$0.05");
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_722), (2026, 9, 26));
    }
}
