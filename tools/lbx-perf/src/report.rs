// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Results model, baseline comparison and the published table (spec T28:
//! "Baselines committed as JSON; nightly workflow compares (regression ⇒
//! tracked issue, non-blocking per §8)").

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;

/// Ratio over the committed baseline that earns a `watch` marker. The
/// baselines are dev-machine numbers and CI runners differ wildly, so the
/// ratio is ADVISORY, only the absolute §6/§7 budgets count as
/// regressions (and file the tracked issue).
const WATCH_RATIO: f64 = 2.0;
/// Metrics below this absolute value are never ratio-flagged (noise floor
/// a 0.2 ms page query doubling to 0.4 ms is not a signal).
const NOISE_FLOOR_MS: f64 = 5.0;

/// One scenario's metrics (name → value; `*_ms` convention for durations).
pub type Metrics = BTreeMap<String, f64>;

/// The full run: scenario → metrics, plus free-form string metadata.
#[derive(Default)]
pub struct Results {
    pub meta: BTreeMap<String, String>,
    pub scenarios: BTreeMap<String, Metrics>,
}

impl Results {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "meta": self.meta,
            "scenarios": self.scenarios,
        })
    }
}

/// Committed baseline: dev-machine numbers + the absolute M0 budgets
/// (§6/§7: page-query p95 < 100 ms @100 k, nav-swap p95 < 50 ms).
pub struct Baseline {
    pub scenarios: BTreeMap<String, Metrics>,
    /// `"<scenario>.<metric>"` → absolute budget in the metric's unit.
    pub budgets: BTreeMap<String, f64>,
}

pub fn load_baseline(path: &Path) -> anyhow::Result<Baseline> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading baseline {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing baseline {}", path.display()))?;
    let mut scenarios: BTreeMap<String, Metrics> = BTreeMap::new();
    if let Some(map) = v.get("scenarios").and_then(|s| s.as_object()) {
        for (name, metrics) in map {
            let mut m = Metrics::new();
            if let Some(obj) = metrics.as_object() {
                for (k, val) in obj {
                    if let Some(f) = val.as_f64() {
                        m.insert(k.clone(), f);
                    }
                }
            }
            scenarios.insert(name.clone(), m);
        }
    }
    let mut budgets = BTreeMap::new();
    if let Some(map) = v.get("budgets").and_then(|s| s.as_object()) {
        for (k, val) in map {
            if let Some(f) = val.as_f64() {
                budgets.insert(k.clone(), f);
            }
        }
    }
    Ok(Baseline { scenarios, budgets })
}

/// Renders the markdown table and returns the number of regressions.
/// Written to stdout, the nightly workflow appends it to the job summary.
pub fn publish_table(results: &Results, baseline: Option<&Baseline>) -> usize {
    let mut regressions = 0;
    println!("### Perf scenarios (E01 T28)\n");
    for (k, v) in &results.meta {
        println!("- {k}: {v}");
    }
    println!();
    println!("| scenario | metric | value | baseline | ratio | budget | status |");
    println!("|---|---|---:|---:|---:|---:|---|");
    for (scenario, metrics) in &results.scenarios {
        for (metric, value) in metrics {
            let base = baseline
                .and_then(|b| b.scenarios.get(scenario))
                .and_then(|m| m.get(metric))
                .copied();
            let budget = baseline
                .and_then(|b| b.budgets.get(&format!("{scenario}.{metric}")))
                .copied();
            let ratio = base.map(|b| if b > 0.0 { value / b } else { f64::NAN });
            let over_budget = budget.is_some_and(|b| *value > b);
            let over_ratio = ratio.is_some_and(|r| r > WATCH_RATIO)
                && *value > NOISE_FLOOR_MS
                && metric.ends_with("_ms");
            let status = if over_budget {
                regressions += 1;
                "REGRESSION (budget)"
            } else if over_ratio {
                "watch (>2× baseline)"
            } else {
                "ok"
            };
            println!(
                "| {scenario} | {metric} | {value:.2} | {} | {} | {} | {status} |",
                base.map_or_else(|| "—".to_owned(), |b| format!("{b:.2}")),
                ratio.map_or_else(|| "—".to_owned(), |r| format!("{r:.2}×")),
                budget.map_or_else(|| "—".to_owned(), |b| format!("{b:.0}")),
            );
        }
    }
    println!();
    if regressions > 0 {
        // The literal token the nightly workflow greps to file the tracked
        // issue (non-blocking per spec §8).
        println!("REGRESSION: {regressions} metric(s) regressed — see table above.");
    } else {
        println!("All metrics within baseline ratio and §7 budgets.");
    }
    regressions
}

/// Nearest-rank percentile (matches the shell overlay's convention).
pub fn percentile(samples: &[f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.95), 95.0);
        assert_eq!(percentile(&v, 0.5), 50.0);
        assert_eq!(percentile(&[], 0.95), 0.0);
    }

    #[test]
    fn only_budget_violations_count_as_regressions() {
        let mut results = Results::default();
        let mut m = Metrics::new();
        m.insert("p95_ms".into(), 120.0); // over the 100 budget AND >2× base
        m.insert("slow_ms".into(), 40.0); // >2× baseline, no budget → watch
        m.insert("fine_ms".into(), 1.0); // 10× baseline but under noise floor
        results.scenarios.insert("page-query-100k".into(), m);

        let mut baseline = Baseline {
            scenarios: BTreeMap::new(),
            budgets: BTreeMap::new(),
        };
        baseline
            .budgets
            .insert("page-query-100k.p95_ms".into(), 100.0);
        let mut bm = Metrics::new();
        bm.insert("p95_ms".into(), 10.0);
        bm.insert("slow_ms".into(), 10.0);
        bm.insert("fine_ms".into(), 0.1);
        baseline.scenarios.insert("page-query-100k".into(), bm);

        // Exactly one regression (the budget violation); the baseline ratio
        // is advisory (`watch`) because baselines are machine-relative.
        assert_eq!(publish_table(&results, Some(&baseline)), 1);
        assert_eq!(publish_table(&results, None), 0, "no baseline, no verdict");
    }
}
