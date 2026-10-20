//! Sample storage.
//!
//! Every 256 samples we keep a small summary (min, max, sum, sum of squares).
//! That makes two things fast no matter how long the capture is:
//!   * drawing: a pixel column covering 100 000 samples only needs ~400 summaries
//!   * statistics for any range (selection / visible window)

pub const BLOCK: usize = 256;
/// ~16 bytes per sample -> about 480 MB when full (8.3 h at 1 kHz).
pub const MAX_SAMPLES: usize = 30_000_000;

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
    pub const EMPTY: Agg = Agg {
        count: 0,
        sum: 0.0,
        sumsq: 0.0,
        min: f64::INFINITY,
        max: f64::NEG_INFINITY,
        min_at: 0,
        max_at: 0,
    };

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

pub struct DataStore {
    /// Sample index (normally 0,1,2,... but CSV files may have gaps).
    pub x: Vec<f64>,
    /// Current in µA.
    pub i: Vec<f64>,
    blocks: Vec<Agg>,
    pub total: Agg,
    pub next_index: f64,
}

impl Default for DataStore {
    fn default() -> Self {
        Self {
            x: Vec::with_capacity(1 << 20),
            i: Vec::with_capacity(1 << 20),
            blocks: Vec::new(),
            total: Agg::EMPTY,
            next_index: 0.0,
        }
    }
}

impl DataStore {
    pub fn len(&self) -> usize {
        self.i.len()
    }

    pub fn is_empty(&self) -> bool {
        self.i.is_empty()
    }

    pub fn clear(&mut self) {
        self.x.clear();
        self.i.clear();
        self.blocks.clear();
        self.total = Agg::EMPTY;
        self.next_index = 0.0;
    }

    pub fn bytes(&self) -> usize {
        (self.x.capacity() + self.i.capacity()) * 8 + self.blocks.capacity() * std::mem::size_of::<Agg>()
    }

    #[inline]
    fn push(&mut self, x: f64, v: f64) {
        let idx = self.i.len();
        if idx % BLOCK == 0 {
            self.blocks.push(Agg::EMPTY);
        }
        self.x.push(x);
        self.i.push(v);
        self.blocks.last_mut().unwrap().add(idx, v);
        self.total.add(idx, v);
    }

    /// Append samples; returns how many fit under MAX_SAMPLES.
    pub fn append(&mut self, chunk: &[f64]) -> usize {
        let k = chunk.len().min(MAX_SAMPLES - self.len());
        self.x.reserve(k);
        self.i.reserve(k);
        for &v in &chunk[..k] {
            let x = self.next_index;
            self.push(x, v);
            self.next_index += 1.0;
        }
        k
    }

    pub fn load(&mut self, x: Vec<f64>, i: Vec<f64>) {
        self.clear();
        let n = i.len().min(MAX_SAMPLES);
        // fresh buffers so memory from a bigger previous capture is released
        self.x = Vec::with_capacity(n.max(1024));
        self.i = Vec::with_capacity(n.max(1024));
        self.blocks = Vec::with_capacity(n / BLOCK + 1);
        for k in 0..n {
            self.push(x[k], i[k]);
        }
        self.next_index = if n > 0 { self.x[n - 1] + 1.0 } else { 0.0 };
    }

    pub fn first_x(&self) -> f64 {
        self.x.first().copied().unwrap_or(0.0)
    }

    pub fn last_x(&self) -> f64 {
        self.x.last().copied().unwrap_or(0.0)
    }

    /// Indices [a, b) of samples whose x lies within [x0, x1].
    pub fn index_range(&self, x0: f64, x1: f64) -> (usize, usize) {
        let a = self.x.partition_point(|&v| v < x0);
        let b = self.x.partition_point(|&v| v <= x1);
        (a, b.max(a))
    }

    pub fn nearest(&self, xv: f64) -> Option<usize> {
        let n = self.len();
        if n == 0 {
            return None;
        }
        let k = self.x.partition_point(|&v| v < xv);
        if k >= n {
            return Some(n - 1);
        }
        if k > 0 && (self.x[k - 1] - xv).abs() < (self.x[k] - xv).abs() {
            return Some(k - 1);
        }
        Some(k)
    }

    /// Statistics of samples [a, b).
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
            for k in a..b {
                r.add(k, self.i[k]);
            }
            return r;
        }
        for k in a..first_full * BLOCK {
            r.add(k, self.i[k]);
        }
        for blk in &self.blocks[first_full..last_full] {
            r.merge(blk);
        }
        for k in last_full * BLOCK..b {
            r.add(k, self.i[k]);
        }
        r
    }

    /// Reduce samples [a, b) to about `buckets` min/max pairs for drawing.
    /// Returns (x, current µA) points. Short spikes are never lost.
    pub fn decimate(&self, a: usize, b: usize, buckets: usize, out: &mut Vec<(f64, f64)>) {
        out.clear();
        let b = b.min(self.len());
        if a >= b {
            return;
        }
        let n = b - a;
        let buckets = buckets.max(1);
        if n <= buckets * 3 {
            out.extend((a..b).map(|k| (self.x[k], self.i[k])));
            return;
        }
        out.reserve(buckets * 2);
        for j in 0..buckets {
            let s = a + n * j / buckets;
            let e = a + n * (j + 1) / buckets;
            if s >= e {
                continue;
            }
            let g = self.agg(s, e);
            let (p, q) = if g.min_at <= g.max_at { (g.min_at, g.max_at) } else { (g.max_at, g.min_at) };
            out.push((self.x[p], self.i[p]));
            if q != p {
                out.push((self.x[q], self.i[q]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agg_matches_naive() {
        let mut d = DataStore::default();
        let v: Vec<f64> = (0..10_000).map(|k| ((k * 7919) % 1000) as f64 - 300.0).collect();
        d.append(&v);
        for &(a, b) in &[(0, 10_000), (3, 9_999), (100, 300), (255, 257), (0, 1), (512, 768)] {
            let g = d.agg(a, b);
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
        d.decimate(0, d.len(), 1000, &mut out);
        assert!(out.len() <= 2000);
        assert!(out.iter().any(|p| p.1 == 5000.0));
    }
}
