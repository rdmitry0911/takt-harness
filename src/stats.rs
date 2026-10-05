//! Summary statistics: median, spread and percentile-bootstrap confidence intervals.

use serde::{Deserialize, Serialize};

/// Bootstrap resamples per interval.
pub const RESAMPLES: u32 = 10_000;
/// Confidence level of every interval.
pub const CI_LEVEL: f64 = 0.95;
/// Fixed seed, so the same samples always give the same interval.
pub const SEED: u64 = 0x7a4b_7e11_0c0f_fee5;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Stats {
    pub n: usize,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    /// Median absolute deviation from the median.
    pub mad: f64,
    /// (max − min) / median.
    pub rel_spread: f64,
    /// 95% percentile-bootstrap confidence interval of the median: [low, high].
    pub ci95: [f64; 2],
}

/// Median of a sample (mean of the two middle values for an even count); NaN when empty.
pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    median_sorted(&s)
}

fn median_sorted(s: &[f64]) -> f64 {
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Linear-interpolation quantile of sorted data, q in [0, 1].
fn quantile_sorted(s: &[f64], q: f64) -> f64 {
    let pos = q.clamp(0.0, 1.0) * (s.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
}

/// SplitMix64: a small, well-mixed deterministic generator for resampling.
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform index in 0..n (multiply-shift; bias is below 2^-50 for any realistic n).
    pub fn index(&mut self, n: usize) -> usize {
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }
}

fn resample_median(v: &[f64], rng: &mut SplitMix64, buf: &mut Vec<f64>) -> f64 {
    buf.clear();
    buf.extend((0..v.len()).map(|_| v[rng.index(v.len())]));
    buf.sort_by(f64::total_cmp);
    median_sorted(buf)
}

fn interval(mut est: Vec<f64>, level: f64) -> [f64; 2] {
    est.sort_by(f64::total_cmp);
    let a = (1.0 - level) / 2.0;
    [quantile_sorted(&est, a), quantile_sorted(&est, 1.0 - a)]
}

/// Percentile-bootstrap confidence interval of the median.
pub fn bootstrap_median_ci(v: &[f64], resamples: u32, level: f64, seed: u64) -> [f64; 2] {
    if v.is_empty() {
        return [f64::NAN, f64::NAN];
    }
    let mut rng = SplitMix64::new(seed);
    let mut buf = Vec::with_capacity(v.len());
    let est = (0..resamples.max(1)).map(|_| resample_median(v, &mut rng, &mut buf)).collect();
    interval(est, level)
}

/// Percentile-bootstrap confidence interval of median(a) / median(b), resampling a and b
/// independently.
pub fn bootstrap_ratio_ci(a: &[f64], b: &[f64], resamples: u32, level: f64, seed: u64) -> [f64; 2] {
    if a.is_empty() || b.is_empty() {
        return [f64::NAN, f64::NAN];
    }
    let mut rng = SplitMix64::new(seed);
    let (mut ba, mut bb) = (Vec::with_capacity(a.len()), Vec::with_capacity(b.len()));
    let est = (0..resamples.max(1))
        .map(|_| {
            let ma = resample_median(a, &mut rng, &mut ba);
            let mb = resample_median(b, &mut rng, &mut bb);
            ma / mb
        })
        .collect();
    interval(est, level)
}

/// All summary statistics of one metric. `None` for an empty sample.
pub fn summarize(v: &[f64]) -> Option<Stats> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let med = median_sorted(&s);
    let (min, max) = (s[0], s[s.len() - 1]);
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    let dev: Vec<f64> = s.iter().map(|x| (x - med).abs()).collect();
    Some(Stats {
        n: s.len(),
        median: med,
        min,
        max,
        mean,
        mad: median(&dev),
        rel_spread: if med != 0.0 { (max - min) / med } else { 0.0 },
        ci95: bootstrap_median_ci(&s, RESAMPLES, CI_LEVEL, SEED),
    })
}

/// Measurement quality grade from the number of repetitions and the spread of the primary metric,
/// measured robustly as MAD / median so that one outlier does not decide the grade (outliers get a
/// separate warning).
///
/// good: at least 9 repetitions and MAD / median of at most 1%;
/// fair: at least 5 repetitions and MAD / median of at most 3%;
/// poor: anything else.
pub fn grade(reps: usize, rel_mad: f64) -> &'static str {
    if reps >= 9 && rel_mad <= 0.01 {
        "good"
    } else if reps >= 5 && rel_mad <= 0.03 {
        "fair"
    } else {
        "poor"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_even_empty() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[7.0]), 7.0);
        assert!(median(&[]).is_nan());
    }

    #[test]
    fn quantiles_interpolate() {
        let s = [0.0, 10.0, 20.0, 30.0, 40.0];
        assert_eq!(quantile_sorted(&s, 0.0), 0.0);
        assert_eq!(quantile_sorted(&s, 1.0), 40.0);
        assert_eq!(quantile_sorted(&s, 0.5), 20.0);
        assert!((quantile_sorted(&s, 0.125) - 5.0).abs() < 1e-12);
    }

    #[test]
    fn summarize_basic() {
        let st = summarize(&[100.0, 102.0, 98.0, 101.0, 99.0]).unwrap();
        assert_eq!(st.n, 5);
        assert_eq!(st.median, 100.0);
        assert_eq!(st.min, 98.0);
        assert_eq!(st.max, 102.0);
        assert_eq!(st.mean, 100.0);
        assert_eq!(st.mad, 1.0);
        assert!((st.rel_spread - 0.04).abs() < 1e-12);
        assert!(st.ci95[0] <= st.median && st.median <= st.ci95[1]);
        assert!(st.ci95[0] >= 98.0 && st.ci95[1] <= 102.0);
        assert!(summarize(&[]).is_none());
    }

    #[test]
    fn constant_sample_has_degenerate_interval() {
        let st = summarize(&[5.0; 9]).unwrap();
        assert_eq!(st.ci95, [5.0, 5.0]);
        assert_eq!(st.rel_spread, 0.0);
        assert_eq!(st.mad, 0.0);
    }

    #[test]
    fn zero_median_has_zero_spread() {
        assert_eq!(summarize(&[0.0, 0.0, 0.0]).unwrap().rel_spread, 0.0);
    }

    #[test]
    fn bootstrap_is_deterministic_and_bounded() {
        let v: Vec<f64> = (0..9).map(|i| 1000.0 + (i * 37 % 11) as f64).collect();
        let a = bootstrap_median_ci(&v, RESAMPLES, CI_LEVEL, SEED);
        let b = bootstrap_median_ci(&v, RESAMPLES, CI_LEVEL, SEED);
        assert_eq!(a, b);
        let m = median(&v);
        assert!(a[0] <= m && m <= a[1], "{a:?} vs {m}");
        let (lo, hi) = (v.iter().cloned().fold(f64::INFINITY, f64::min), v.iter().cloned().fold(0.0, f64::max));
        assert!(a[0] >= lo && a[1] <= hi);
    }

    #[test]
    fn bootstrap_interval_narrows_with_more_samples() {
        // The same distribution (a repeating pattern), 9 vs 99 samples.
        let small: Vec<f64> = (0..9).map(|i| 100.0 + (i % 9) as f64).collect();
        let large: Vec<f64> = (0..99).map(|i| 100.0 + (i % 9) as f64).collect();
        let ws = bootstrap_median_ci(&small, RESAMPLES, CI_LEVEL, SEED);
        let wl = bootstrap_median_ci(&large, RESAMPLES, CI_LEVEL, SEED);
        assert!(wl[1] - wl[0] < ws[1] - ws[0], "{wl:?} vs {ws:?}");
    }

    #[test]
    fn ratio_interval_contains_point_estimate() {
        let a: Vec<f64> = (0..9).map(|i| 1000.0 + i as f64).collect();
        let b: Vec<f64> = (0..9).map(|i| 250.0 + (i % 3) as f64).collect();
        let ci = bootstrap_ratio_ci(&a, &b, RESAMPLES, CI_LEVEL, SEED);
        let r = median(&a) / median(&b);
        assert!(ci[0] <= r && r <= ci[1], "{ci:?} vs {r}");
        assert!(ci[0] > 3.9 && ci[1] < 4.1);
        let same = bootstrap_ratio_ci(&[2.0; 5], &[1.0; 5], 100, CI_LEVEL, SEED);
        assert_eq!(same, [2.0, 2.0]);
    }

    #[test]
    fn rng_index_in_range_and_spread() {
        let mut r = SplitMix64::new(1);
        let mut hits = [0u32; 7];
        for _ in 0..70_000 {
            hits[r.index(7)] += 1;
        }
        assert!(hits.iter().all(|&h| (9_000..11_000).contains(&h)), "{hits:?}");
    }

    #[test]
    fn grades() {
        assert_eq!(grade(9, 0.005), "good");
        assert_eq!(grade(9, 0.02), "fair");
        assert_eq!(grade(5, 0.005), "fair");
        assert_eq!(grade(4, 0.0), "poor");
        assert_eq!(grade(20, 0.05), "poor");
    }
}
