//! Capture files.
//!
//! Format (one reading per line; voltage and sample period stored once):
//!
//! ```text
//! # Analizator capture
//! # voltage_V=3.3
//! # sample_period_ms=1
//! # start_index=0
//! # saved=2026-10-20 18:37:27
//! # marker=1500,radio TX
//! Current_uA
//! 0
//! 1750.435
//! ```
//!
//! Power is not stored: it is current × voltage. The sample index is the line
//! number (+ start_index). Optional extra column on export: `Current_filtered_uA`.
//! If the samples are not evenly spaced (rare: imported files with gaps), an
//! `Index` column is written first.
//!
//! Older files (`Index,Current_uA,Voltage_V,Power_uW` on every line) and plain
//! one-column files still load.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct Meta {
    pub voltage: f64,
    pub period_ms: f64,
    /// (sample index, label)
    pub markers: Vec<(f64, String)>,
}

/// `filtered`: optional extra column with the filtered current (export only).
pub fn save(path: &Path, x: &[f64], i: &[f64], filtered: Option<&[f64]>, meta: &Meta) -> Result<usize, String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
    }
    let f = fs::File::create(path).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let werr = |e: std::io::Error| format!("Write error: {e}");

    let start = x.first().copied().unwrap_or(0.0);
    let contiguous = x.iter().enumerate().all(|(k, &v)| v == start + k as f64);
    let mut header = format!(
        "# Analizator capture\n# voltage_V={}\n# sample_period_ms={}\n# start_index={}\n# saved={}\n",
        meta.voltage,
        meta.period_ms,
        start as i64,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
    );
    for (mx, label) in &meta.markers {
        let label: String = label.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        header.push_str(&format!("# marker={},{}\n", mx.round() as i64, label.trim()));
    }
    header.push_str(&format!(
        "{}Current_uA{}\n",
        if contiguous { "" } else { "Index," },
        if filtered.is_some() { ",Current_filtered_uA" } else { "" },
    ));
    w.write_all(header.as_bytes()).map_err(werr)?;

    let mut line = Vec::with_capacity(64 * 1024);
    for (k, &iv) in i.iter().enumerate() {
        if !contiguous {
            push_int(&mut line, x[k].round() as i64);
            line.push(b',');
        }
        push_exact(&mut line, iv);
        if let Some(f) = filtered {
            line.push(b',');
            push_trimmed(&mut line, f[k], 4);
        }
        line.push(b'\n');
        if line.len() > 60 * 1024 {
            w.write_all(&line).map_err(werr)?;
            line.clear();
        }
    }
    w.write_all(&line).map_err(werr)?;
    w.flush().map_err(werr)?;
    Ok(i.len())
}

/// Shortest text that reads back as exactly the same number ("0", "1750.435").
fn push_exact(out: &mut Vec<u8>, v: f64) {
    if v == v.trunc() && v.abs() < 1e15 {
        push_int(out, v as i64);
    } else {
        let _ = write!(out, "{v}");
    }
}

fn push_int(out: &mut Vec<u8>, v: i64) {
    if v < 0 {
        out.push(b'-');
    }
    let mut u = v.unsigned_abs();
    let mut tmp = [0u8; 20];
    let mut k = tmp.len();
    loop {
        k -= 1;
        tmp[k] = b'0' + (u % 10) as u8;
        u /= 10;
        if u == 0 {
            break;
        }
    }
    out.extend_from_slice(&tmp[k..]);
}

/// Up to `dec` decimals, trailing zeros removed ("5.12", "0").
fn push_trimmed(out: &mut Vec<u8>, v: f64, dec: u32) {
    let m = 10u64.pow(dec);
    let scaled = v * m as f64;
    if !scaled.is_finite() || scaled.abs() >= 9.0e15 {
        let _ = write!(out, "{v}");
        return;
    }
    let r = scaled.round() as i64;
    if r < 0 {
        out.push(b'-');
    }
    let a = r.unsigned_abs();
    push_int(out, (a / m) as i64);
    let mut frac = a % m;
    if frac == 0 {
        return;
    }
    let mut digits = dec;
    while frac % 10 == 0 {
        frac /= 10;
        digits -= 1;
    }
    out.push(b'.');
    let mut d = 10u64.pow(digits - 1);
    while d > 0 {
        out.push(b'0' + ((frac / d) % 10) as u8);
        d /= 10;
    }
}

pub struct Loaded {
    pub x: Vec<f64>,
    pub i: Vec<f64>,
    pub voltage: Option<f64>,
    pub period_ms: Option<f64>,
    pub markers: Vec<(f64, String)>,
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let bytes = fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut voltage = None;
    let mut period_ms = None;
    let mut markers = Vec::new();
    let mut start_index = 0.0;
    // column positions; None until a header (or the first data line) tells us
    let mut col_x: Option<usize> = None;
    let mut col_i: Option<usize> = None;
    let mut col_v: Option<usize> = None;
    let mut layout_known = false;
    let mut x = Vec::new();
    let mut i = Vec::new();
    let mut bad = 0usize;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(c) = line.strip_prefix('#') {
            if let Some((k, raw)) = c.split_once('=') {
                let v = raw.trim().parse::<f64>().ok();
                match k.trim() {
                    "voltage_V" => voltage = v,
                    "sample_period_ms" => period_ms = v,
                    "start_index" => start_index = v.unwrap_or(0.0),
                    "marker" => {
                        let (pos, label) = raw.split_once(',').unwrap_or((raw, ""));
                        if let Ok(p) = pos.trim().parse::<f64>() {
                            markers.push((p, label.trim().to_string()));
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        let cols: Vec<&str> = line.split([',', ';', '\t']).map(str::trim).collect();
        if !layout_known {
            let is_header = cols.iter().any(|c| c.chars().any(|ch| ch.is_ascii_alphabetic()) && c.parse::<f64>().is_err());
            if is_header {
                for (k, name) in cols.iter().enumerate() {
                    let n = name.to_ascii_lowercase();
                    if n.starts_with("index") || n == "sample" {
                        col_x = Some(k);
                    } else if n.starts_with("current") && !n.contains("filtered") && col_i.is_none() {
                        col_i = Some(k);
                    } else if n.starts_with("voltage") {
                        col_v = Some(k);
                    }
                }
                if col_i.is_none() {
                    return Err("No current column found in this file.".into());
                }
                layout_known = true;
                continue;
            }
            // no header: one column = current; two or more = index, current
            if cols.len() == 1 {
                col_i = Some(0);
            } else {
                col_x = Some(0);
                col_i = Some(1);
            }
            layout_known = true;
        }
        let get = |c: Option<usize>| c.and_then(|k| cols.get(k)).and_then(|s| s.parse::<f64>().ok());
        match get(col_i) {
            Some(cur) => {
                let xi = match col_x {
                    Some(_) => match get(col_x) {
                        Some(v) => v,
                        None => {
                            bad += 1;
                            continue;
                        }
                    },
                    None => start_index + i.len() as f64,
                };
                if voltage.is_none() {
                    voltage = get(col_v);
                }
                x.push(xi);
                i.push(cur);
            }
            None => bad += 1,
        }
    }
    if i.is_empty() {
        return Err("The file contains no samples.".into());
    }
    if bad > i.len() {
        return Err("This does not look like an Analizator capture file.".into());
    }
    // make sure x is sorted (needed for fast lookups)
    if x.windows(2).any(|w| w[1] < w[0]) {
        let mut idx: Vec<usize> = (0..x.len()).collect();
        idx.sort_by(|&a, &b| x[a].total_cmp(&x[b]));
        x = idx.iter().map(|&k| x[k]).collect();
        i = idx.iter().map(|&k| i[k]).collect();
    }
    markers.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(Loaded { x, i, voltage, period_ms, markers })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("analizator_test");
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn meta() -> Meta {
        Meta { voltage: 3.3, period_ms: 0.5, markers: vec![(12.0, "radio TX".into()), (11.0, "line\nbreak".into())] }
    }

    #[test]
    fn roundtrip_new_format() {
        let p = tmp("new.csv");
        let i = [0.0, 1750.435, 2.25, -0.0001, 123456.789012];
        save(&p, &[10.0, 11.0, 12.0, 13.0, 14.0], &i, None, &meta()).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains("# voltage_V=3.3\n") && text.contains("\nCurrent_uA\n0\n1750.435\n"), "{text}");
        let l = load(&p).unwrap();
        assert_eq!(l.i, i.to_vec()); // exact
        assert_eq!(l.x, vec![10.0, 11.0, 12.0, 13.0, 14.0]);
        assert_eq!(l.voltage, Some(3.3));
        assert_eq!(l.period_ms, Some(0.5));
        assert_eq!(l.markers, vec![(11.0, "line break".to_string()), (12.0, "radio TX".to_string())]);
    }

    #[test]
    fn filtered_column_and_gaps() {
        let p = tmp("filt.csv");
        save(&p, &[0.0, 1.0, 5.0], &[1.5, 2.0, 3.0], Some(&[1.23456, 2.0, 0.1]), &meta()).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains("\nIndex,Current_uA,Current_filtered_uA\n0,1.5,1.2346\n1,2,2\n5,3,0.1\n"), "{text}");
        let l = load(&p).unwrap();
        assert_eq!(l.x, vec![0.0, 1.0, 5.0]);
        assert_eq!(l.i, vec![1.5, 2.0, 3.0]);
    }

    #[test]
    fn loads_old_format_and_plain_column() {
        let p = tmp("old.csv");
        fs::write(&p, "Index,Current_uA,Voltage_V,Power_uW\n0,1.5000,3.300,4.9500\n1,2.2500,3.300,7.4250\n").unwrap();
        let l = load(&p).unwrap();
        assert_eq!((l.x, l.i, l.voltage), (vec![0.0, 1.0], vec![1.5, 2.25], Some(3.3)));
        let p = tmp("plain.csv");
        fs::write(&p, "5\n6\n7\n").unwrap();
        let l = load(&p).unwrap();
        assert_eq!((l.x, l.i), (vec![0.0, 1.0, 2.0], vec![5.0, 6.0, 7.0]));
    }

    #[test]
    fn trimmed_format() {
        let f = |v| {
            let mut b = Vec::new();
            push_trimmed(&mut b, v, 4);
            String::from_utf8(b).unwrap()
        };
        assert_eq!(f(0.0), "0");
        assert_eq!(f(5.12), "5.12");
        assert_eq!(f(-2.5), "-2.5");
        assert_eq!(f(0.00004), "0");
        assert_eq!(f(1234.56789), "1234.5679");
        assert_eq!(f(0.0501), "0.0501");
    }
}

#[cfg(test)]
mod bench {
    #[test]
    #[ignore]
    fn load_big() {
        let p = std::env::var("BIG_CSV").unwrap();
        let t = std::time::Instant::now();
        let l = super::load(std::path::Path::new(&p)).unwrap();
        eprintln!("loaded {} rows in {:.2} s", l.i.len(), t.elapsed().as_secs_f64());
        let t = std::time::Instant::now();
        let out = std::env::temp_dir().join("big_out.csv");
        super::save(&out, &l.x, &l.i, None, &super::Meta { voltage: 3.3, period_ms: 1.0, markers: vec![] }).unwrap();
        eprintln!("saved in {:.2} s, {} MB", t.elapsed().as_secs_f64(), std::fs::metadata(&out).unwrap().len() / 1_000_000);
        let t = std::time::Instant::now();
        let l2 = super::load(&out).unwrap();
        eprintln!("re-loaded new format in {:.2} s", t.elapsed().as_secs_f64());
        assert_eq!(l2.i.len(), l.i.len());
    }
}
