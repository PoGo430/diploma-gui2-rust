//! Number formatting helpers (SI prefixes etc.)

const SI: [(f64, &str); 7] = [(1e6, "M"), (1e3, "k"), (1.0, ""), (1e-3, "m"), (1e-6, "µ"), (1e-9, "n"), (1e-12, "p")];

/// Like Python's "%.{digits}g": `digits` significant digits, trailing zeros removed.
pub fn sig(v: f64, digits: i32) -> String {
    if v == 0.0 || !v.is_finite() {
        return format!("{v}");
    }
    let mag = v.abs().log10().floor() as i32;
    let decimals = (digits - 1 - mag).max(0) as usize;
    let mut s = format!("{v:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// 1.234e-3, "A" -> "1.234 mA"
pub fn si(value: f64, unit: &str) -> String {
    si_digits(value, unit, 4)
}

pub fn si_digits(value: f64, unit: &str, digits: i32) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    let a = value.abs();
    if a < 1e-15 {
        return format!("0 {unit}");
    }
    let (factor, prefix) = SI.iter().find(|(f, _)| a >= *f * 0.99995).copied().unwrap_or((1e-12, "p"));
    let mut s = sig(value / factor, digits);
    // rounding can push 999.96 -> "1000"; move up a prefix in that case
    if s.trim_start_matches('-') == "1000"
        && let Some(pos) = SI.iter().position(|(f, _)| *f == factor)
        && pos > 0
    {
        let (f2, p2) = SI[pos - 1];
        s = sig(value / f2, digits);
        return format!("{s} {p2}{unit}");
    }
    format!("{s} {prefix}{unit}")
}

pub fn duration(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "—".into();
    }
    if seconds < 60.0 {
        return si(seconds, "s");
    }
    let total = seconds;
    let h = (total / 3600.0).floor() as u64;
    let m = ((total - h as f64 * 3600.0) / 60.0).floor() as u64;
    let s = total - h as f64 * 3600.0 - m as f64 * 60.0;
    if h > 0 { format!("{h}:{m:02}:{s:04.1}") } else { format!("{m}:{s:04.1}") }
}

/// 1234567 -> "1 234 567"
pub fn count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (k, ch) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Battery runtime: "45 min", "17.5 h", "41 days", "2.3 years".
pub fn runtime(hours: f64) -> String {
    if !hours.is_finite() {
        return "—".into();
    }
    if hours < 1.0 {
        format!("{:.0} min", hours * 60.0)
    } else if hours < 48.0 {
        format!("{} h", sig(hours, 3))
    } else if hours < 24.0 * 730.0 {
        format!("{} days", sig(hours / 24.0, 3))
    } else {
        format!("{} years", sig(hours / 24.0 / 365.25, 3))
    }
}

/// Time-axis label: "250 ms", "12.5 s", "1:15", "1:02:03"; sample numbers when `time` is false.
pub fn axis_label(v: f64, time: bool) -> String {
    if !time {
        return sig(v, 9);
    }
    if v.abs() < 1e-12 {
        return "0".into();
    }
    if v.abs() < 60.0 {
        return si_digits(v, "s", 4);
    }
    let sign = if v < 0.0 { "-" } else { "" };
    let t = v.abs();
    let whole = t.round();
    let (h, m, s) = ((whole / 3600.0).floor(), ((whole % 3600.0) / 60.0).floor(), whole % 60.0);
    let frac = if (t - whole).abs() > 1e-6 {
        sig(t - t.floor(), 3).trim_start_matches('0').to_string()
    } else {
        String::new()
    };
    if h > 0.0 { format!("{sign}{h}:{m:02}:{s:02}{frac}") } else { format!("{sign}{m}:{s:02}{frac}") }
}

/// Like [`axis_label`], but empty for the label at the very right edge (it would be cut off).
pub fn axis_label_edge(v: f64, range: &std::ops::RangeInclusive<f64>, time: bool) -> String {
    let span = range.end() - range.start();
    if v > range.end() - span * 0.025 {
        return String::new();
    }
    axis_label(v, time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(si(437.9e-6, "A"), "437.9 µA");
        assert_eq!(si(0.01012, "A"), "10.12 mA");
        assert_eq!(si(2.17, "s"), "2.17 s");
        assert_eq!(si(0.0, "A"), "0 A");
        assert_eq!(si(-0.002, "A"), "-2 mA");
        assert_eq!(si(0.99999, "A"), "1 A");
        assert_eq!(count(5147), "5 147");
        assert_eq!(count(999), "999");
        assert_eq!(duration(75.5), "1:15.5");
        assert_eq!(runtime(0.5), "30 min");
        assert_eq!(runtime(17.5), "17.5 h");
        assert_eq!(runtime(24.0 * 41.0), "41 days");
        assert_eq!(axis_label(0.25, true), "250 ms");
        assert_eq!(axis_label(75.0, true), "1:15");
        assert_eq!(axis_label(3723.0, true), "1:02:03");
        assert_eq!(axis_label(1500.0, false), "1500");
    }
}
