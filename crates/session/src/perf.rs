//! 性能の測定（T-054 / T-068 / T-096）。測る道具だけで、ふだんの動きには関わらない。
//!
//! [`Samples`] は時間（マイクロ秒）を溜めて、件数・平均・分位点・最大を出す。溜めるのは [`Samples::CAP`] 件まで
//! （それを超えたら件数・合計・最大だけを数え続け、分位点は最初の `CAP` 件から求める）。

use std::time::Duration;

/// 時間の標本
#[derive(Clone, Debug)]
pub struct Samples {
    name: &'static str,
    values: Vec<u32>,
    count: u64,
    total_us: u64,
    max_us: u32,
}

/// [`Samples::summary`] の結果（マイクロ秒）
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: u64,
    pub mean_us: f64,
    pub p50_us: u32,
    pub p90_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
}

impl Samples {
    /// 溜める件数の上限（u32 で 4 MB）
    pub const CAP: usize = 1_000_000;

    pub const fn new(name: &'static str) -> Self {
        Self { name, values: Vec::new(), count: 0, total_us: 0, max_us: 0 }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn add(&mut self, d: Duration) {
        self.add_us(d.as_micros().min(u32::MAX as u128) as u32);
    }

    pub fn add_us(&mut self, us: u32) {
        if self.values.len() < Self::CAP {
            self.values.push(us);
        }
        self.count += 1;
        self.total_us += us as u64;
        self.max_us = self.max_us.max(us);
    }

    pub fn clear(&mut self) {
        self.values.clear();
        self.count = 0;
        self.total_us = 0;
        self.max_us = 0;
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn summary(&self) -> Summary {
        if self.count == 0 {
            return Summary::default();
        }
        let mut v = self.values.clone();
        v.sort_unstable();
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        Summary {
            count: self.count,
            mean_us: self.total_us as f64 / self.count as f64,
            p50_us: at(0.5),
            p90_us: at(0.9),
            p99_us: at(0.99),
            max_us: self.max_us,
        }
    }

    /// 1 行の報告（ms）。例: `draw n=120 mean=1.20 p50=1.10 p90=1.50 p99=2.30 max=3.10`
    pub fn line(&self) -> String {
        let s = self.summary();
        let ms = |us: u32| us as f64 / 1000.0;
        format!(
            "{} n={} mean={:.2} p50={:.2} p90={:.2} p99={:.2} max={:.2}",
            self.name,
            s.count,
            s.mean_us / 1000.0,
            ms(s.p50_us),
            ms(s.p90_us),
            ms(s.p99_us),
            ms(s.max_us)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut s = Samples::new("x");
        for i in 1..=100 {
            s.add_us(i * 10);
        }
        let m = s.summary();
        assert_eq!(m.count, 100);
        assert_eq!(m.max_us, 1000);
        assert_eq!(m.p50_us, 510);
        assert_eq!(m.p99_us, 990);
        assert!((m.mean_us - 505.0).abs() < 1e-9);
        assert!(s.line().starts_with("x n=100 "));
        s.clear();
        assert_eq!(s.summary(), Summary::default());
    }
}
