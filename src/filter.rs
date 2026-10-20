//! Display filters. They never change the recorded data; a filtered copy is
//! kept next to it (see `DataStore`).

use serde::{Deserialize, Serialize};

pub const MAX_AVG_WINDOW: usize = 100_001;
pub const MAX_MEDIAN_WINDOW: usize = 2_001;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterKind {
    Off,
    /// Centered moving average over `window` samples.
    MovingAverage,
    /// Centered median over `window` samples (removes spikes/glitches, keeps edges).
    Median,
    /// First-order low-pass (like an RC filter), cut-off in Hz.
    LowPass,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterCfg {
    pub kind: FilterKind,
    /// Window length in samples (always odd).
    pub window: usize,
    pub cutoff_hz: f64,
}

impl Default for FilterCfg {
    fn default() -> Self {
        Self { kind: FilterKind::Off, window: 21, cutoff_hz: 50.0 }
    }
}

impl FilterCfg {
    pub fn active(&self) -> bool {
        self.kind != FilterKind::Off
    }

    /// Window clamped to a valid odd size for the current filter type.
    pub fn sanitized(mut self) -> Self {
        let max = if self.kind == FilterKind::Median { MAX_MEDIAN_WINDOW } else { MAX_AVG_WINDOW };
        self.window = self.window.clamp(3, max) | 1;
        if !(self.cutoff_hz > 0.0) {
            self.cutoff_hz = 1.0;
        }
        self
    }

    /// How many samples before a position can change when new data arrives.
    pub fn lookahead(&self) -> usize {
        match self.kind {
            FilterKind::MovingAverage | FilterKind::Median => self.window / 2,
            _ => 0,
        }
    }

    /// Short description for plot headers, e.g. "moving average · 21 samples".
    pub fn describe(&self, dt: f64) -> String {
        match self.kind {
            FilterKind::Off => String::new(),
            FilterKind::MovingAverage => {
                format!("moving average · {} samples ({})", self.window, crate::fmt::si(self.window as f64 * dt, "s"))
            }
            FilterKind::Median => format!("median · {} samples ({})", self.window, crate::fmt::si(self.window as f64 * dt, "s")),
            FilterKind::LowPass => format!("low-pass · {}", crate::fmt::si(self.cutoff_hz, "Hz")),
        }
    }
}

/// Filter `x[start..]`. `prev` is the filtered value at `start - 1` (needed by the low-pass).
/// Window filters are centered and shrink near the ends of the data.
pub fn apply(cfg: &FilterCfg, dt: f64, x: &[f64], start: usize, prev: Option<f64>) -> Vec<f64> {
    let n = x.len();
    if start >= n {
        return Vec::new();
    }
    match cfg.kind {
        FilterKind::Off => x[start..].to_vec(),
        FilterKind::MovingAverage => moving_average(x, cfg.window / 2, start),
        FilterKind::Median => median(x, cfg.window / 2, start),
        FilterKind::LowPass => {
            // exact discretisation of an RC low-pass: y += a * (x - y)
            let a = 1.0 - (-std::f64::consts::TAU * cfg.cutoff_hz * dt).exp();
            let mut y = prev.unwrap_or(x[start]);
            x[start..]
                .iter()
                .map(|&v| {
                    y += a * (v - y);
                    y
                })
                .collect()
        }
    }
}

fn moving_average(x: &[f64], h: usize, start: usize) -> Vec<f64> {
    let n = x.len();
    let mut out = Vec::with_capacity(n - start);
    let mut lo = start.saturating_sub(h);
    let mut hi = (start + h + 1).min(n);
    let mut sum: f64 = x[lo..hi].iter().sum();
    for k in start..n {
        if k > start {
            let nlo = k.saturating_sub(h);
            let nhi = (k + h + 1).min(n);
            if nlo > lo {
                sum -= x[lo];
                lo = nlo;
            }
            if nhi > hi {
                sum += x[hi];
                hi = nhi;
            }
            // re-sum now and then so rounding errors can't build up
            if k % 65_536 == 0 {
                sum = x[lo..hi].iter().sum();
            }
        }
        out.push(sum / (hi - lo) as f64);
    }
    out
}

fn median(x: &[f64], h: usize, start: usize) -> Vec<f64> {
    let n = x.len();
    let mut out = Vec::with_capacity(n - start);
    let mut lo = start.saturating_sub(h);
    let mut hi = (start + h + 1).min(n);
    let mut win: Vec<f64> = x[lo..hi].to_vec();
    win.sort_by(f64::total_cmp);
    for k in start..n {
        if k > start {
            let nlo = k.saturating_sub(h);
            let nhi = (k + h + 1).min(n);
            if nlo > lo {
                let p = win.partition_point(|v| v.total_cmp(&x[lo]).is_lt());
                win.remove(p);
                lo = nlo;
            }
            if nhi > hi {
                let p = win.partition_point(|v| v.total_cmp(&x[hi]).is_lt());
                win.insert(p, x[hi]);
                hi = nhi;
            }
        }
        let m = win.len();
        out.push(if m % 2 == 1 { win[m / 2] } else { 0.5 * (win[m / 2 - 1] + win[m / 2]) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(x: &[f64], h: usize, med: bool) -> Vec<f64> {
        (0..x.len())
            .map(|k| {
                let lo = k.saturating_sub(h);
                let hi = (k + h + 1).min(x.len());
                let mut w = x[lo..hi].to_vec();
                if med {
                    w.sort_by(f64::total_cmp);
                    let m = w.len();
                    if m % 2 == 1 { w[m / 2] } else { 0.5 * (w[m / 2 - 1] + w[m / 2]) }
                } else {
                    w.iter().sum::<f64>() / w.len() as f64
                }
            })
            .collect()
    }

    #[test]
    fn window_filters_match_naive() {
        let x: Vec<f64> = (0..500).map(|k| ((k * 7919) % 101) as f64 + if k == 250 { 1e4 } else { 0.0 }).collect();
        for &h in &[1usize, 2, 5, 30] {
            for &start in &[0usize, 1, 17, 499] {
                let ma = moving_average(&x, h, start);
                let md = median(&x, h, start);
                let (na, nm) = (naive(&x, h, false), naive(&x, h, true));
                for (k, v) in ma.iter().enumerate() {
                    assert!((v - na[start + k]).abs() < 1e-9);
                }
                assert_eq!(md, nm[start..].to_vec());
            }
        }
    }

    #[test]
    fn median_removes_spike() {
        let mut x = vec![5.0; 100];
        x[50] = 10_000.0;
        let cfg = FilterCfg { kind: FilterKind::Median, window: 5, cutoff_hz: 0.0 };
        assert!(apply(&cfg, 1e-3, &x, 0, None).iter().all(|&v| v == 5.0));
    }

    #[test]
    fn lowpass_step_response() {
        // 1 kHz sampling, 10 Hz cut-off: after one time constant (~15.9 ms) about 63 %
        let x = vec![1.0; 1000];
        let cfg = FilterCfg { kind: FilterKind::LowPass, window: 3, cutoff_hz: 10.0 };
        let y = apply(&cfg, 1e-3, &x, 0, Some(0.0));
        let tau_samples = (1.0 / (std::f64::consts::TAU * 10.0) / 1e-3).round() as usize;
        assert!((y[tau_samples - 1] - 0.632).abs() < 0.02, "{}", y[tau_samples - 1]);
        // continuing from the middle gives the same result
        let y2 = apply(&cfg, 1e-3, &x, 500, Some(y[499]));
        assert!((y2[0] - y[500]).abs() < 1e-12);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    #[ignore]
    fn speed() {
        let n = 10_000_000;
        let x: Vec<f64> = (0..n).map(|k| ((k * 7919) % 997) as f64).collect();
        for (kind, w) in [(FilterKind::MovingAverage, 1001), (FilterKind::LowPass, 3), (FilterKind::Median, 21), (FilterKind::Median, 201), (FilterKind::Median, 2001)] {
            let cfg = FilterCfg { kind, window: w, cutoff_hz: 10.0 };
            let t = std::time::Instant::now();
            let y = apply(&cfg, 1e-3, &x, 0, None);
            eprintln!("{kind:?} w={w}: {:.0} ms ({} samples)", t.elapsed().as_secs_f64() * 1e3, y.len());
        }
    }
}
