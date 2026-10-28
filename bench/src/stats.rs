//! Small statistics helpers used by the suites.

/// Latency or duration distribution summary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

/// Nearest-rank percentile of an ascending-sorted slice. `p` is in `[0, 100]`.
///
/// Returns `None` for an empty slice.
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let p = p.clamp(0.0, 100.0);
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[idx])
}

/// Summarise samples. Non-finite samples are ignored. Returns `None` if nothing is left.
pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let mut v: Vec<f64> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    Some(Summary {
        count: v.len(),
        mean,
        p50: percentile(&v, 50.0)?,
        p95: percentile(&v, 95.0)?,
        max: *v.last()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), Some(50.0));
        assert_eq!(percentile(&v, 95.0), Some(95.0));
        assert_eq!(percentile(&v, 100.0), Some(100.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[7.0], 99.0), Some(7.0));
    }

    #[test]
    fn summarize_ignores_non_finite() {
        let s = summarize(&[3.0, f64::NAN, 1.0, 2.0, f64::INFINITY]).unwrap();
        assert_eq!(s.count, 3);
        assert_eq!(s.p50, 2.0);
        assert_eq!(s.max, 3.0);
        assert!((s.mean - 2.0).abs() < 1e-12);
        assert!(summarize(&[f64::NAN]).is_none());
    }
}
