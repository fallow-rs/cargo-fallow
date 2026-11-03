//! The health score of a Rust workspace.
//!
//! The model is the health score of fallow for JavaScript: the score starts at
//! 100, each penalty subtracts points, the result is rounded to one decimal and
//! clamped to 0..=100, and a letter grade comes from the whole number part.
//! Each penalty comes from one Rust check. A check that did not run gives no
//! penalty, and the penalty is not in the report. The score is for one
//! language: the report has `language: "rust"`, and no score combines
//! languages.
//!
//! | Penalty | Input | Formula | Cap |
//! | --- | --- | --- | --- |
//! | `dead_public` | `dead_public` findings per `pub` item, in percent | `pct * 0.2` | 15 |
//! | `complexity` | functions with cyclomatic complexity of 50 or more, in percent | `pct * 4` | 20 |
//! | `unit_size` | functions over 60 lines, per 1000 functions | `per_mille * 0.5` | 10 |
//! | `unused_deps` | `error` findings of `deps`, per 1000 source files | `per_k * 0.5` | 25 |
//! | `boundary_violations` | boundary violations, per 1000 source files | `per_k * 0.5` | 25 |
//! | `duplication` | duplicated lines, in percent | `pct - 5` | 10 |

use serde::Serialize;

/// The version of the Rust formula. A change of a weight, a cap or an input
/// increments it.
pub const FORMULA_VERSION: u32 = 1;
/// The score with no penalty.
const MAX_SCORE: f64 = 100.0;
/// A function with this cyclomatic complexity or more is critical. The same
/// value as the critical band of the complexity check and of fallow.
pub const CRITICAL_CYCLOMATIC: u32 = 50;
/// A function over this many lines counts for `unit_size`, the same as in
/// fallow.
pub const LARGE_FUNCTION_LINES: u32 = 60;
/// The duplicated percentage that gives no penalty.
const DUPLICATION_ALLOWANCE: f64 = 5.0;

/// The weight and the cap of one penalty.
struct Rule {
    weight: f64,
    cap: f64,
}

const DEAD_PUBLIC: Rule = Rule {
    weight: 0.2,
    cap: 15.0,
};
const COMPLEXITY: Rule = Rule {
    weight: 4.0,
    cap: 20.0,
};
const UNIT_SIZE: Rule = Rule {
    weight: 0.5,
    cap: 10.0,
};
const UNUSED_DEPS: Rule = Rule {
    weight: 0.5,
    cap: 25.0,
};
const BOUNDARY_VIOLATIONS: Rule = Rule {
    weight: 0.5,
    cap: 25.0,
};
const DUPLICATION_CAP: f64 = 10.0;

/// The letter grades, from the best. A score at or over the bound gets the
/// letter. A lower score gets `F`.
const GRADES: [(u32, &str); 4] = [(85, "A"), (70, "B"), (55, "C"), (40, "D")];

/// What the checks of one run measured. A field is `None` when its check did
/// not run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Metrics {
    /// The source files that the module walk reached.
    pub total_files: usize,
    /// The functions and closures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functions: Option<usize>,
    /// The functions with cyclomatic complexity of [`CRITICAL_CYCLOMATIC`] or more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_functions: Option<usize>,
    /// The functions over [`LARGE_FUNCTION_LINES`] lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_functions: Option<usize>,
    /// The `pub` items in the module graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_items: Option<usize>,
    /// The `dead_public` findings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dead_public: Option<usize>,
    /// The `error` findings of `deps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unused_deps: Option<usize>,
    /// The boundary violations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary_violations: Option<usize>,
    /// The duplicated lines as a percentage of all lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplication_percentage: Option<f64>,
}

/// The points that each penalty subtracts. A penalty is `None` when its check
/// did not run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Penalties {
    /// From `visibility`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dead_public: Option<f64>,
    /// From `complexity`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity: Option<f64>,
    /// From `size`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_size: Option<f64>,
    /// From `deps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unused_deps: Option<f64>,
    /// From `boundaries`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary_violations: Option<f64>,
    /// From `dupes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplication: Option<f64>,
}

impl Penalties {
    fn values(&self) -> [Option<f64>; 6] {
        [
            self.dead_public,
            self.complexity,
            self.unit_size,
            self.unused_deps,
            self.boundary_violations,
            self.duplication,
        ]
    }
}

/// The health score of one language. The field order is the wire order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthScore {
    /// Always `rust`.
    pub language: &'static str,
    /// The version of the formula.
    pub formula_version: u32,
    /// The score, from 0 to 100, with one decimal.
    pub score: f64,
    /// `A`, `B`, `C`, `D` or `F`.
    pub grade: &'static str,
    /// The points that each penalty subtracts.
    pub penalties: Penalties,
    /// The inputs of the penalties.
    pub metrics: Metrics,
}

/// Computes the score, or `None` when no check with a penalty ran.
pub fn score(metrics: Metrics) -> Option<HealthScore> {
    let penalties = penalties(&metrics);
    if penalties.values().iter().all(Option::is_none) {
        return None;
    }
    let total: f64 = penalties.values().iter().flatten().sum();
    let score = round1(MAX_SCORE - total).clamp(0.0, MAX_SCORE);
    Some(HealthScore {
        language: "rust",
        formula_version: FORMULA_VERSION,
        score,
        grade: grade(score),
        penalties,
        metrics,
    })
}

fn penalties(metrics: &Metrics) -> Penalties {
    let files = metrics.total_files;
    let functions = metrics.functions.unwrap_or(0);
    Penalties {
        dead_public: pair(metrics.dead_public, metrics.public_items)
            .map(|(dead, items)| weighted(&DEAD_PUBLIC, percent(dead, items))),
        complexity: metrics
            .critical_functions
            .map(|count| weighted(&COMPLEXITY, percent(count, functions))),
        unit_size: metrics
            .large_functions
            .map(|count| weighted(&UNIT_SIZE, per_mille(count, functions))),
        unused_deps: metrics
            .unused_deps
            .map(|count| weighted(&UNUSED_DEPS, per_mille(count, files))),
        boundary_violations: metrics
            .boundary_violations
            .map(|count| weighted(&BOUNDARY_VIOLATIONS, per_mille(count, files))),
        duplication: metrics
            .duplication_percentage
            .map(|pct| round1((pct - DUPLICATION_ALLOWANCE).clamp(0.0, DUPLICATION_CAP))),
    }
}

const fn pair(a: Option<usize>, b: Option<usize>) -> Option<(usize, usize)> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    }
}

fn weighted(rule: &Rule, value: f64) -> f64 {
    round1((value * rule.weight).min(rule.cap))
}

/// `part` as a percentage of `whole`, with one decimal. Zero when `whole` is
/// zero.
fn percent(part: usize, whole: usize) -> f64 {
    ratio(part, whole, 100.0)
}

/// `part` per 1000 of `whole`, with one decimal. Zero when `whole` is zero.
fn per_mille(part: usize, whole: usize) -> f64 {
    ratio(part, whole, 1000.0)
}

fn ratio(part: usize, whole: usize, scale: f64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        round1(part as f64 / whole as f64 * scale)
    }
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The letter of a score. The whole number part decides, so 84.9 is a `B`.
fn grade(score: f64) -> &'static str {
    #[expect(
        clippy::cast_sign_loss,
        reason = "the score is clamped to 0..=100 before the call"
    )]
    let whole = score as u32;
    GRADES
        .iter()
        .find(|(bound, _)| whole >= *bound)
        .map_or("F", |(_, letter)| letter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_checks() -> Metrics {
        Metrics {
            total_files: 200,
            functions: Some(1000),
            critical_functions: Some(0),
            large_functions: Some(0),
            public_items: Some(500),
            dead_public: Some(0),
            unused_deps: Some(0),
            boundary_violations: Some(0),
            duplication_percentage: Some(0.0),
        }
    }

    fn total(metrics: Metrics) -> f64 {
        score(metrics).map_or(-1.0, |score| score.score)
    }

    #[test]
    fn a_clean_workspace_scores_100_and_gets_an_a() {
        let health = score(all_checks());
        assert_eq!(health.as_ref().map(|h| h.score), Some(100.0));
        assert_eq!(health.as_ref().map(|h| h.grade), Some("A"));
        assert_eq!(health.map(|h| h.language), Some("rust"));
    }

    #[test]
    fn no_score_without_a_check_that_has_a_penalty() {
        let metrics = Metrics {
            total_files: 10,
            ..Metrics::default()
        };
        assert_eq!(score(metrics), None);
    }

    #[test]
    fn each_penalty_follows_its_formula() {
        let health = score(metrics_with_all_penalties()).map(|h| h.penalties);
        assert_eq!(
            health,
            Some(Penalties {
                dead_public: Some(2.0),
                complexity: Some(4.0),
                // 15 points, over the cap of 10.
                unit_size: Some(10.0),
                unused_deps: Some(5.0),
                boundary_violations: Some(2.5),
                duplication: Some(2.5),
            })
        );
        // 100 - (2 + 4 + 10 + 5 + 2.5 + 2.5).
        assert_eq!(total(metrics_with_all_penalties()), 74.0);
    }

    /// 50 of 500 pub items are dead (10 %), 10 of 1000 functions are critical
    /// (1 %), 30 of 1000 functions are large (30 per mille), 2 unused
    /// dependencies and 1 violation in 200 files (10 and 5 per 1000 files),
    /// and 7.5 % of the lines are duplicated.
    fn metrics_with_all_penalties() -> Metrics {
        Metrics {
            dead_public: Some(50),
            critical_functions: Some(10),
            large_functions: Some(30),
            unused_deps: Some(2),
            boundary_violations: Some(1),
            duplication_percentage: Some(7.5),
            ..all_checks()
        }
    }

    #[test]
    fn each_penalty_stops_at_its_cap_and_the_score_at_zero() {
        let metrics = Metrics {
            total_files: 1,
            functions: Some(1),
            critical_functions: Some(1),
            large_functions: Some(1),
            public_items: Some(1),
            dead_public: Some(1),
            unused_deps: Some(100),
            boundary_violations: Some(100),
            duplication_percentage: Some(100.0),
        };
        let health = score(metrics);
        assert_eq!(
            health.as_ref().map(|h| h.penalties.clone()),
            Some(Penalties {
                dead_public: Some(15.0),
                complexity: Some(20.0),
                unit_size: Some(10.0),
                unused_deps: Some(25.0),
                boundary_violations: Some(25.0),
                duplication: Some(10.0),
            })
        );
        // The caps sum to 105, so the clamp gives 0.
        assert_eq!(health.as_ref().map(|h| h.score), Some(0.0));
        assert_eq!(health.map(|h| h.grade), Some("F"));
    }

    #[test]
    fn a_check_that_did_not_run_has_no_penalty() {
        let metrics = Metrics {
            total_files: 100,
            unused_deps: Some(1),
            ..Metrics::default()
        };
        let health = score(metrics);
        assert_eq!(
            health.as_ref().map(|h| h.penalties.clone()),
            Some(Penalties {
                unused_deps: Some(5.0),
                ..Penalties::default()
            })
        );
        assert_eq!(health.map(|h| h.score), Some(95.0));
    }

    #[test]
    fn zero_inputs_give_no_division_by_zero() {
        let metrics = Metrics {
            total_files: 0,
            functions: Some(0),
            public_items: Some(0),
            ..all_checks()
        };
        assert_eq!(total(metrics), 100.0);
    }

    #[test]
    fn grades_use_the_whole_number_part() {
        assert_eq!(grade(100.0), "A");
        assert_eq!(grade(85.0), "A");
        assert_eq!(grade(84.9), "B");
        assert_eq!(grade(70.0), "B");
        assert_eq!(grade(55.0), "C");
        assert_eq!(grade(40.0), "D");
        assert_eq!(grade(39.9), "F");
        assert_eq!(grade(0.0), "F");
    }

    #[test]
    fn duplication_under_the_allowance_gives_no_points() {
        let metrics = Metrics {
            duplication_percentage: Some(4.9),
            ..all_checks()
        };
        assert_eq!(
            score(metrics).and_then(|h| h.penalties.duplication),
            Some(0.0)
        );
    }
}
