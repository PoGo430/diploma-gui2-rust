//! Sample storage.
//!
//! Every 256 samples we keep a small summary (min, max, sum, sum of squares).
//! That makes two things fast no matter how long the capture is:
//!   * drawing: a pixel column covering 100 000 samples only needs ~400 summaries
//!   * statistics for any range (selection / visible window)
//!
//! The raw samples are never modified. When a filter is active, a filtered
//! copy (with its own summaries) is kept alongside and updated as data arrives.

use crate::filter::{self, FilterCfg};

pub const BLOCK: usize = 256;
/// ~8 bytes per sample (+8 with a filter) -> about 400 MB when full
/// (13.9 h at 1 kHz, 8.3 min at the PPK2's 100 kHz).
pub const MAX_SAMPLES: usize = 50_000_000;

#[derive(Clone, Copy, Debug)]
pub struct Agg {
    pub count: usize,
    pub sum: f64,
    pub sumsq: f64,
    pub min: f64,
    pub max: f64,
    pub min_at: usize,
    pub max_at: usize,
}

impl Agg {
    pub const EMPTY: Agg =
        Agg { count: 0, sum: 0.0, sumsq: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY, min_at: 0, max_at: 0 };

    #[inline]
    fn add(&mut self, idx: usize, v: f64) {
        self.count += 1;
        self.sum += v;
        self.sumsq += v * v;
        if v < self.min {
            self.min = v;
            self.min_at = idx;
        }
        if v > self.max {
            self.max = v;
            self.max_at = idx;
        }
    }

    #[inline]
    fn merge(&mut self, o: &Agg) {
        if o.count == 0 {
            return;
        }
        self.count += o.count;
        self.sum += o.sum;
        self.sumsq += o.sumsq;
        if o.min < self.min {
            self.min = o.min;
            self.min_at = o.min_at;
        }
        if o.max > self.max {
            self.max = o.max;
            self.max_at = o.max_at;
        }
    }
}

/// A list of values plus per-block summaries.
#[derive(Default)]
pub struct Series {
    pub v: Vec<f64>,
    blocks: Vec<Agg>,
    pub total: Agg,
}

impl Default for Agg {
    fn default() -> Self {
        Agg::EMPTY
    }
}

impl Series {
    fn with_capacity(n: usize) -> Self {
        Self { v: Vec::with_capacity(n), blocks: Vec::with_capacity(n / BLOCK + 1), total: Agg::EMPTY }
    }

    pub fn len(&self) -> usize {
        self.v.len()
    }

    fn clear(&mut self) {
        self.v.clear();
        self.blocks.clear();
        self.total = Agg::EMPTY;
    }

    #[inline]
    fn push(&mut self, val: f64) {
        let idx = self.v.len();
        if idx.is_multiple_of(BLOCK) {
            self.blocks.push(Agg::EMPTY);
        }
        self.v.push(val);
        self.blocks.last_mut().unwrap().add(idx, val);
        self.total.add(idx, val);
    }

    fn extend(&mut self, vals: &[f64]) {
        self.v.reserve(vals.len());
        for &val in vals {
            self.push(val);
        }
    }

    /// Drop everything from `len` on (used to recompute the end of a filtered series).
    fn truncate(&mut self, len: usize) {
        if len >= self.v.len() {
            return;
        }
        self.v.truncate(len);
        let nb = len.div_ceil(BLOCK);
        self.blocks.truncate(nb);
        if !len.is_multiple_of(BLOCK) {
            let s = (nb - 1) * BLOCK;
            let mut b = Agg::EMPTY;
            for (k, &v) in self.v[s..len].iter().enumerate() {
                b.add(s + k, v);
            }
            self.blocks[nb - 1] = b;
        }
        let mut t = Agg::EMPTY;
        for b in &self.blocks {
            t.merge(b);
        }
        self.total = t;
    }

    /// Statistics of values [a, b).
    pub fn agg(&self, a: usize, b: usize) -> Agg {
        let b = b.min(self.len());
        let mut r = Agg::EMPTY;
        if a >= b {
            return r;
        }
        if a == 0 && b == self.len() {
            return self.total;
        }
        let first_full = a.div_ceil(BLOCK);
        let last_full = b / BLOCK; // exclusive
        if first_full >= last_full {
            for (k, &v) in self.v[a..b].iter().enumerate() {
                r.add(a + k, v);
            }
            return r;
        }
        for (k, &v) in self.v[a..first_full * BLOCK].iter().enumerate() {
            r.add(a + k, v);
        }
        for blk in &self.blocks[first_full..last_full] {
            r.merge(blk);
        }
        let tail = last_full * BLOCK;
        for (k, &v) in self.v[tail..b].iter().enumerate() {
            r.add(tail + k, v);
        }
        r
    }

    /// (count, sum) of the values below `thr`, and of those at or above it, in [a, b).
    pub fn split(&self, a: usize, b: usize, thr: f64) -> ((usize, f64), (usize, f64)) {
        let b = b.min(self.len());
        let (mut lo, mut hi) = ((0usize, 0.0f64), (0usize, 0.0f64));
        if a >= b {
            return (lo, hi);
        }
        for &v in &self.v[a..b] {
            if v < thr {
                lo.0 += 1;
                lo.1 += v;
            } else {
                hi.0 += 1;
                hi.1 += v;
            }
        }
        (lo, hi)
    }

    fn bytes(&self) -> usize {
        self.v.capacity() * 8 + self.blocks.capacity() * std::mem::size_of::<Agg>()
    }
}

/// Sample positions (x). Recordings are evenly spaced (start, start+1, …) and
/// need no storage; only imported files with gaps keep an explicit list.
enum Positions {
    Even { start: f64 },
    Listed(Vec<f64>),
}

pub struct DataStore {
    pos: Positions,
    /// Current in µA, exactly as received.
    pub raw: Series,
    /// Filtered current in µA (same length as `raw` while a filter is active, else empty).
    pub filt: Series,
    filter: FilterCfg,
    dt: f64,
    pub next_index: f64,
    /// Changes whenever the data changes (for caching derived results).
    pub generation: u64,
}

impl Default for DataStore {
    fn default() -> Self {
        Self {
            pos: Positions::Even { start: 0.0 },
            raw: Series::with_capacity(1 << 20),
            filt: Series::default(),
            filter: FilterCfg::default(),
            dt: 1e-3,
            next_index: 0.0,
            generation: 0,
        }
    }
}

impl DataStore {
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.v.is_empty()
    }

    pub fn clear(&mut self) {
        self.pos = Positions::Even { start: 0.0 };
        self.raw.clear();
        self.filt.clear();
        self.next_index = 0.0;
        self.generation += 1;
    }

    pub fn bytes(&self) -> usize {
        let pos = match &self.pos {
            Positions::Even { .. } => 0,
            Positions::Listed(v) => v.capacity() * 8,
        };
        pos + self.raw.bytes() + self.filt.bytes()
    }

    pub fn filter_active(&self) -> bool {
        self.filter.active()
    }

    /// The series to show: filtered when a filter is active (and `filtered` is asked for).
    pub fn series(&self, filtered: bool) -> &Series {
        if filtered && self.filter.active() { &self.filt } else { &self.raw }
    }

    /// Change filter settings / sample period; recomputes the filtered copy if needed.
    pub fn set_filter(&mut self, cfg: FilterCfg, dt: f64) {
        let cfg = cfg.sanitized();
        let dt_matters = cfg.kind == filter::FilterKind::LowPass;
        if cfg == self.filter && (!dt_matters || dt == self.dt) {
            self.dt = dt;
            return;
        }
        self.filter = cfg;
        self.dt = dt;
        self.recompute_filter();
    }

    fn recompute_filter(&mut self) {
        self.generation += 1;
        self.filt.clear();
        if self.filter.active() && !self.raw.v.is_empty() {
            let vals = filter::apply(&self.filter, self.dt, &self.raw.v, 0, None);
            self.filt = Series::with_capacity(self.raw.v.capacity());
            self.filt.extend(&vals);
        } else {
            self.filt = Series::default();
        }
    }

    /// After new raw samples from `old_n` on: update the end of the filtered series.
    fn extend_filter(&mut self, old_n: usize) {
        if !self.filter.active() {
            return;
        }
        let start = old_n.saturating_sub(self.filter.lookahead()).min(self.filt.len());
        self.filt.truncate(start);
        let prev = start.checked_sub(1).map(|k| self.filt.v[k]);
        let vals = filter::apply(&self.filter, self.dt, &self.raw.v, start, prev);
        self.filt.extend(&vals);
    }

    /// Append samples; returns how many fit under MAX_SAMPLES.
    pub fn append(&mut self, chunk: &[f64]) -> usize {
        let old_n = self.len();
        let k = chunk.len().min(MAX_SAMPLES - old_n);
        self.raw.v.reserve(k);
        for &v in &chunk[..k] {
            if let Positions::Listed(x) = &mut self.pos {
                x.push(self.next_index);
            }
            self.raw.push(v);
            self.next_index += 1.0;
        }
        self.extend_filter(old_n);
        self.generation += 1;
        k
    }

    /// Replace the data. `x` must be sorted; evenly spaced positions are not stored.
    pub fn load(&mut self, x: Vec<f64>, i: Vec<f64>) {
        let n = i.len().min(MAX_SAMPLES).min(x.len());
        let even = x[..n].iter().enumerate().all(|(k, &v)| v == x[0] + k as f64);
        self.pos = if n == 0 || even {
            Positions::Even { start: x.first().copied().unwrap_or(0.0) }
        } else {
            let mut x = x;
            x.truncate(n);
            x.shrink_to_fit();
            Positions::Listed(x)
        };
        // fresh buffers so memory from a bigger previous capture is released
        self.raw = Series::with_capacity(n.max(1024));
        self.raw.extend(&i[..n]);
        self.next_index = if n > 0 { self.x_at(n - 1) + 1.0 } else { 0.0 };
        self.recompute_filter();
    }

    /// Sample index (x position) of sample `k`.
    #[inline]
    pub fn x_at(&self, k: usize) -> f64 {
        match &self.pos {
            Positions::Even { start } => start + k as f64,
            Positions::Listed(x) => x[k],
        }
    }

    /// Sample indices of samples [a, b).
    pub fn x_range(&self, a: usize, b: usize) -> Vec<f64> {
        match &self.pos {
            Positions::Even { start } => (a..b).map(|k| start + k as f64).collect(),
            Positions::Listed(x) => x[a..b].to_vec(),
        }
    }

    pub fn first_x(&self) -> f64 {
        if self.is_empty() { 0.0 } else { self.x_at(0) }
    }

    pub fn last_x(&self) -> f64 {
        if self.is_empty() { 0.0 } else { self.x_at(self.len() - 1) }
    }

    /// Indices [a, b) of samples whose x lies within [x0, x1].
    pub fn index_range(&self, x0: f64, x1: f64) -> (usize, usize) {
        let n = self.len();
        let (a, b) = match &self.pos {
            Positions::Even { start } => {
                let idx = |v: f64| if v.is_nan() { 0 } else { v.clamp(0.0, n as f64) as usize };
                (idx((x0 - start).ceil()), idx((x1 - start).floor() + 1.0))
            }
            Positions::Listed(x) => (x.partition_point(|&v| v < x0), x.partition_point(|&v| v <= x1)),
        };
        (a, b.max(a))
    }

    pub fn nearest(&self, xv: f64) -> Option<usize> {
        let n = self.len();
        if n == 0 || xv.is_nan() {
            return None;
        }
        match &self.pos {
            Positions::Even { start } => Some((xv - start).round().clamp(0.0, (n - 1) as f64) as usize),
            Positions::Listed(x) => {
                let k = x.partition_point(|&v| v < xv);
                if k >= n {
                    return Some(n - 1);
                }
                if k > 0 && (x[k - 1] - xv).abs() < (x[k] - xv).abs() {
                    return Some(k - 1);
                }
                Some(k)
            }
        }
    }

    /// Reduce samples [a, b) of series `s` to about `buckets` min/max pairs for drawing.
    /// Returns (x, current µA) points. Short spikes are never lost.
    pub fn decimate(&self, s: &Series, a: usize, b: usize, buckets: usize, out: &mut Vec<(f64, f64)>) {
        out.clear();
        let b = b.min(s.len()).min(self.len());
        if a >= b {
            return;
        }
        let n = b - a;
        let buckets = buckets.max(1);
        if n <= buckets * 3 {
            out.extend((a..b).map(|k| (self.x_at(k), s.v[k])));
            return;
        }
        out.reserve(buckets * 2);
        for j in 0..buckets {
            let st = a + n * j / buckets;
            let en = a + n * (j + 1) / buckets;
            if st >= en {
                continue;
            }
            let g = s.agg(st, en);
            let (p, q) = if g.min_at <= g.max_at { (g.min_at, g.max_at) } else { (g.max_at, g.min_at) };
            out.push((self.x_at(p), s.v[p]));
            if q != p {
                out.push((self.x_at(q), s.v[q]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::FilterKind;

    #[test]
    fn agg_matches_naive() {
        let mut d = DataStore::default();
        let v: Vec<f64> = (0..10_000).map(|k| ((k * 7919) % 1000) as f64 - 300.0).collect();
        d.append(&v);
        for &(a, b) in &[(0, 10_000), (3, 9_999), (100, 300), (255, 257), (0, 1), (512, 768)] {
            let g = d.raw.agg(a, b);
            let s: f64 = v[a..b].iter().sum();
            let mn = v[a..b].iter().cloned().fold(f64::INFINITY, f64::min);
            let mx = v[a..b].iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            assert_eq!(g.count, b - a);
            assert!((g.sum - s).abs() < 1e-6);
            assert_eq!(g.min, mn);
            assert_eq!(g.max, mx);
            assert_eq!(v[g.min_at], mn);
            assert_eq!(v[g.max_at], mx);
        }
    }

    #[test]
    fn decimate_keeps_spikes() {
        let mut d = DataStore::default();
        let mut v = vec![1.0; 1_000_000];
        v[123_457] = 5000.0;
        d.append(&v);
        let mut out = Vec::new();
        d.decimate(&d.raw, 0, d.len(), 1000, &mut out);
        assert!(out.len() <= 2000);
        assert!(out.iter().any(|p| p.1 == 5000.0));
    }

    /// Evenly spaced data (no stored positions) must answer every lookup exactly
    /// like an explicit position list.
    #[test]
    fn even_positions_match_listed() {
        let i: Vec<f64> = (0..1000).map(|k| k as f64).collect();
        let x: Vec<f64> = (0..1000).map(|k| 500.0 + k as f64).collect();
        let mut even = DataStore::default();
        even.load(x.clone(), i.clone());
        assert!(matches!(even.pos, Positions::Even { start } if start == 500.0));
        let mut listed = DataStore { pos: Positions::Listed(x.clone()), ..Default::default() };
        listed.raw.extend(&i);
        for (x0, x1) in
            [(0.0, 10.0), (499.5, 510.2), (600.0, 600.0), (1400.0, f64::INFINITY), (-1e9, 1e9), (2000.0, 3000.0)]
        {
            assert_eq!(even.index_range(x0, x1), listed.index_range(x0, x1), "{x0}..{x1}");
        }
        for xv in [-5.0, 499.4, 500.6, 750.49, 1499.0, 9999.0] {
            assert_eq!(even.nearest(xv), listed.nearest(xv), "{xv}");
        }
        assert_eq!(even.x_range(10, 13), vec![510.0, 511.0, 512.0]);
        assert_eq!((even.first_x(), even.last_x(), even.next_index), (500.0, 1499.0, 1500.0));
        // a file with a gap keeps its positions
        let mut gap = DataStore::default();
        gap.load(vec![0.0, 1.0, 5.0], vec![1.0, 2.0, 3.0]);
        assert_eq!(gap.index_range(2.0, 5.0), (2, 3));
        gap.append(&[4.0]);
        assert_eq!(gap.last_x(), 6.0);
        // recording needs no position storage
        let mut rec = DataStore::default();
        rec.append(&vec![1.0; 10_000]);
        assert_eq!(rec.bytes(), rec.raw.bytes());
    }

    /// Filtering while data streams in must give exactly the same result as
    /// filtering the finished capture.
    #[test]
    fn live_filter_matches_offline() {
        let v: Vec<f64> = (0..5000).map(|k| ((k * 7919) % 997) as f64).collect();
        for kind in [FilterKind::MovingAverage, FilterKind::Median, FilterKind::LowPass] {
            let cfg = FilterCfg { kind, window: 31, cutoff_hz: 20.0 };
            let mut live = DataStore::default();
            live.set_filter(cfg, 1e-3);
            for c in v.chunks(77) {
                live.append(c);
            }
            let mut off = DataStore::default();
            off.append(&v);
            off.set_filter(cfg, 1e-3);
            assert_eq!(live.filt.len(), v.len());
            for k in 0..v.len() {
                assert!((live.filt.v[k] - off.filt.v[k]).abs() < 1e-6, "{kind:?} at {k}");
            }
            let (a, b) = (live.filt.agg(100, 4000), off.filt.agg(100, 4000));
            assert!((a.sum - b.sum).abs() < 1e-3 && a.max == b.max && a.min == b.min);
        }
    }
}
