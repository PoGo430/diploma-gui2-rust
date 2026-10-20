//! CSV files. Same format as the Python version:
//!     Index,Current_uA,Voltage_V,Power_uW

use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

pub fn save(path: &Path, x: &[f64], i: &[f64], voltage: f64) -> Result<usize, String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
    }
    let f = fs::File::create(path).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let werr = |e: std::io::Error| format!("Write error: {e}");
    w.write_all(b"Index,Current_uA,Voltage_V,Power_uW\n").map_err(werr)?;
    let mut v_txt = Vec::new();
    push_fixed(&mut v_txt, voltage, 3);
    let mut line = Vec::with_capacity(64 * 1024);
    for (xv, iv) in x.iter().zip(i) {
        push_int(&mut line, xv.round() as i64);
        line.push(b',');
        push_fixed(&mut line, *iv, 4);
        line.push(b',');
        line.extend_from_slice(&v_txt);
        line.push(b',');
        push_fixed(&mut line, iv * voltage, 4);
        line.push(b'\n');
        if line.len() > 60 * 1024 {
            w.write_all(&line).map_err(werr)?;
            line.clear();
        }
    }
    w.write_all(&line).map_err(werr)?;
    w.flush().map_err(werr)?;
    Ok(x.len())
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

/// Same output as format!("{:.dec$}", v) for normal values, but much faster.
fn push_fixed(out: &mut Vec<u8>, v: f64, dec: u32) {
    let m = 10u64.pow(dec);
    let scaled = v * m as f64;
    if !scaled.is_finite() || scaled.abs() >= 9.0e15 {
        out.extend_from_slice(format!("{v:.prec$}", prec = dec as usize).as_bytes());
        return;
    }
    let r = scaled.round() as i64;
    if r < 0 {
        out.push(b'-');
    }
    let a = r.unsigned_abs();
    push_int(out, (a / m) as i64);
    out.push(b'.');
    let frac = a % m;
    let mut d = m / 10;
    while d > 0 {
        out.push(b'0' + ((frac / d) % 10) as u8);
        d /= 10;
    }
}

pub struct Loaded {
    pub x: Vec<f64>,
    pub i: Vec<f64>,
    pub voltage: Option<f64>,
}

/// Accepts files written by this program, or a plain single column of current values.
pub fn load(path: &Path) -> Result<Loaded, String> {
    let bytes = fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut x = Vec::new();
    let mut i = Vec::new();
    let mut voltage = None;
    let mut bad = 0usize;
    for (ln, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split([',', ';', '\t']).map(str::trim);
        let c0 = cols.next().and_then(|s| s.parse::<f64>().ok());
        let c1 = cols.next().map(|s| s.parse::<f64>().ok());
        let c2 = cols.next().and_then(|s| s.parse::<f64>().ok());
        match (c0, c1) {
            (Some(a), None) => {
                // single column file: current only
                x.push(i.len() as f64);
                i.push(a);
            }
            (Some(a), Some(Some(b))) => {
                x.push(a);
                i.push(b);
                if voltage.is_none() {
                    voltage = c2;
                }
            }
            _ => {
                if ln > 0 {
                    bad += 1;
                }
            }
        }
    }
    if i.is_empty() {
        return Err("The file contains no samples.".into());
    }
    if bad > i.len() {
        return Err("This does not look like an Analizator CSV file.".into());
    }
    // make sure x is sorted (needed for fast lookups)
    if x.windows(2).any(|w| w[1] < w[0]) {
        let mut idx: Vec<usize> = (0..x.len()).collect();
        idx.sort_by(|&a, &b| x[a].total_cmp(&x[b]));
        x = idx.iter().map(|&k| x[k]).collect();
        i = idx.iter().map(|&k| i[k]).collect();
    }
    Ok(Loaded { x, i, voltage })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join("analizator_test");
        let p = dir.join("t.csv");
        save(&p, &[0.0, 1.0, 2.0], &[1.5, 2.25, 1000.0], 3.3).unwrap();
        let l = load(&p).unwrap();
        assert_eq!(l.x, vec![0.0, 1.0, 2.0]);
        assert_eq!(l.i, vec![1.5, 2.25, 1000.0]);
        assert_eq!(l.voltage, Some(3.3));
    }

    #[test]
    fn fixed_format() {
        for &v in &[0.0, 1.5, -2.25, 1234.56789, 0.00004, 0.00005, -0.00004, 9999.99995, 3.3] {
            let mut b = Vec::new();
            push_fixed(&mut b, v, 4);
            let want = format!("{v:.4}");
            let got = String::from_utf8(b).unwrap();
            // may differ by one unit in the last digit on rounding boundaries
            let (g, w): (f64, f64) = (got.parse().unwrap(), want.parse().unwrap());
            assert!((g - w).abs() <= 1.0001e-4, "{v}: {got} vs {want}");
        }
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
        super::save(&out, &l.x, &l.i, 3.3).unwrap();
        eprintln!("saved in {:.2} s", t.elapsed().as_secs_f64());
    }
}
