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
//! | `unit_size` | functions over 60 lines, per 1000 functions | `per_mille * 0.2` | 10 |
//! | `unused_deps` | `error` findings of `deps`, per declared dependency, in percent | `pct * 0.5` | 25 |
//! | `boundary_violations` | boundary violations, per source file, in percent | `pct * 0.5` | 25 |
//! | `duplication` | duplicated lines, in percent | `pct - 5` | 10 |
//!
//! Fallow for JavaScript counts unused dependencies and cycles per 1000
//! files. A Rust workspace has fewer and larger files, so the same rate gives
//! a full penalty for two unused dependencies in 30 files. The Rust formula
//! counts an unused member dependency against the dependency entries of the
//! members, and a boundary violation against the files, in percent.
//!
//! Rust functions are longer than JavaScript functions. With the JavaScript
//! weight of 0.5, `unit_size` reached its cap on six of seven corpus
//! workspaces. The weight 0.2 spreads them from 2.9 to 8.3 points.
//! `docs/health-score.md` has the data.
//!
//! The score and the grade need each check with a penalty. A run without one
//! of them reports the penalties that it has and names the missing checks.

use serde::Serialize;

/// The version of the Rust formula. A change of a weight, a cap or an input
/// increments it.
pub const FORMULA_VERSION: u32 = 2;
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
/// Calibrated on the corpus in `docs/health-score.md`.
const UNIT_SIZE: Rule = Rule {
    weight: 0.2,
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
    /// The functions and closures outside the `exclude` list of `complexity`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity_functions: Option<usize>,
    /// Of those, the functions with cyclomatic complexity of
    /// [`CRITICAL_CYCLOMATIC`] or more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_functions: Option<usize>,
    /// The functions and closures outside the `exclude` list of `size`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_functions: Option<usize>,
    /// Of those, the functions over [`LARGE_FUNCTION_LINES`] lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_functions: Option<usize>,
    /// The `pub` items in the module graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_items: Option<usize>,
    /// The `dead_public` findings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dead_public: Option<usize>,
    /// The dependency entries in the `Cargo.toml` of the members.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_dependencies: Option<usize>,
    /// The `error` findings of `deps` on a member dependency. An unused
    /// `[workspace.dependencies]` entry is not a member dependency, so it
    /// does not count.
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
    /// The check of each penalty, in the order of [`Penalties::values`].
    const CHECKS: [&'static str; 6] = [
        "visibility",
        "complexity",
        "size",
        "deps",
        "boundaries",
        "dupes",
    ];

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
    /// The score, from 0 to 100, with one decimal. `None` when a check of
    /// [`HealthScore::missing_checks`] did not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// `A`, `B`, `C`, `D` or `F`. `None` without a score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade: Option<&'static str>,
    /// The checks with a penalty that the run did not include. A score
    /// without them reads too high, so the report has no score then.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_checks: Vec<&'static str>,
    /// The points that each penalty subtracts.
    pub penalties: Penalties,
    /// The inputs of the penalties.
    pub metrics: Metrics,
}

/// Computes the score, or `None` when no check with a penalty ran. The
/// score and the grade need each check with a penalty.
pub fn score(metrics: Metrics) -> Option<HealthScore> {
    let penalties = penalties(&metrics);
    let values = penalties.values();
    if values.iter().all(Option::is_none) {
        return None;
    }
    let missing_checks: Vec<&'static str> = values
        .iter()
        .zip(Penalties::CHECKS)
        .filter(|(value, _)| value.is_none())
        .map(|(_, check)| check)
        .collect();
    let score = missing_checks.is_empty().then(|| {
        let total: f64 = values.iter().flatten().sum();
        round1(MAX_SCORE - total).clamp(0.0, MAX_SCORE)
    });
    Some(HealthScore {
        language: "rust",
        formula_version: FORMULA_VERSION,
        score,
        grade: score.map(grade),
        missing_checks,
        penalties,
        metrics,
    })
}

fn penalties(metrics: &Metrics) -> Penalties {
    let files = metrics.total_files;
    Penalties {
        dead_public: pair(metrics.dead_public, metrics.public_items)
            .map(|(dead, items)| weighted(&DEAD_PUBLIC, percent(dead, items))),
        complexity: pair(metrics.critical_functions, metrics.complexity_functions)
            .map(|(count, functions)| weighted(&COMPLEXITY, percent(count, functions))),
        unit_size: pair(metrics.large_functions, metrics.size_functions)
            .map(|(count, functions)| weighted(&UNIT_SIZE, per_mille(count, functions))),
        unused_deps: pair(metrics.unused_deps, metrics.declared_dependencies)
            .map(|(count, declared)| weighted(&UNUSED_DEPS, percent(count, declared))),
        boundary_violations: metrics
            .boundary_violations
            .map(|count| weighted(&BOUNDARY_VIOLATIONS, percent(count, files))),
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
            complexity_functions: Some(1000),
            size_functions: Some(1000),
            critical_functions: Some(0),
            large_functions: Some(0),
            public_items: Some(500),
            dead_public: Some(0),
            declared_dependencies: Some(100),
            unused_deps: Some(0),
            boundary_violations: Some(0),
            duplication_percentage: Some(0.0),
        }
    }

    fn total(metrics: Metrics) -> f64 {
        score(metrics).and_then(|score| score.score).unwrap_or(-1.0)
    }

    #[test]
    fn a_clean_workspace_scores_100_and_gets_an_a() {
        let health = score(all_checks());
        assert_eq!(health.as_ref().and_then(|h| h.score), Some(100.0));
        assert_eq!(health.as_ref().and_then(|h| h.grade), Some("A"));
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
                // 30 per 1000 functions, times 0.2.
                unit_size: Some(6.0),
                unused_deps: Some(5.0),
                boundary_violations: Some(2.5),
                duplication: Some(2.5),
            })
        );
        // 100 - (2 + 4 + 6 + 5 + 2.5 + 2.5).
        assert_eq!(total(metrics_with_all_penalties()), 78.0);
    }

    /// The functions and the functions over 60 lines of the seven corpus
    /// workspaces in docs/health-score.md. With the weight of formula
    /// version 1 (0.5), six of the seven reached the cap.
    #[test]
    fn unit_size_separates_the_corpus_workspaces() {
        let corpus = [
            ("ripgrep", 3296, 48, 2.9),
            ("fallow", 69264, 1619, 4.7),
            ("typos", 797, 22, 5.5),
            ("mdbook", 1077, 30, 5.6),
            ("gitui", 2676, 94, 7.0),
            ("taplo", 1111, 46, 8.3),
            ("git-cliff", 674, 28, 8.3),
        ];
        for (name, functions, large, expected) in corpus {
            let metrics = Metrics {
                size_functions: Some(functions),
                large_functions: Some(large),
                ..all_checks()
            };
            let penalty = score(metrics).and_then(|h| h.penalties.unit_size);
            assert_eq!(penalty, Some(expected), "{name}");
            assert!(expected < UNIT_SIZE.cap, "{name}");
        }
    }

    /// 50 of 500 pub items are dead (10 %), 10 of 1000 functions are critical
    /// (1 %), 30 of 1000 functions are large (30 per mille), 10 of 100
    /// dependencies are unused (10 %), 10 violations in 200 files (5 %),
    /// and 7.5 % of the lines are duplicated.
    fn metrics_with_all_penalties() -> Metrics {
        Metrics {
            dead_public: Some(50),
            critical_functions: Some(10),
            large_functions: Some(30),
            unused_deps: Some(10),
            boundary_violations: Some(10),
            duplication_percentage: Some(7.5),
            ..all_checks()
        }
    }

    #[test]
    fn each_penalty_stops_at_its_cap_and_the_score_at_zero() {
        let metrics = Metrics {
            total_files: 1,
            complexity_functions: Some(1),
            size_functions: Some(1),
            critical_functions: Some(1),
            large_functions: Some(1),
            public_items: Some(1),
            dead_public: Some(1),
            declared_dependencies: Some(1),
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
        assert_eq!(health.as_ref().and_then(|h| h.score), Some(0.0));
        assert_eq!(health.and_then(|h| h.grade), Some("F"));
    }

    #[test]
    fn a_check_that_did_not_run_has_no_penalty() {
        let metrics = Metrics {
            total_files: 100,
            declared_dependencies: Some(10),
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
        // A score without five of its six inputs reads too high, so a partial
        // run gets no score and no grade, and names the checks that it needs.
        assert_eq!(health.as_ref().map(|h| h.score), Some(None));
        assert_eq!(health.as_ref().map(|h| h.grade), Some(None));
        assert_eq!(
            health.map(|h| h.missing_checks),
            Some(vec!["visibility", "complexity", "size", "boundaries", "dupes"])
        );
    }

    #[test]
    fn zero_inputs_give_no_division_by_zero() {
        let metrics = Metrics {
            total_files: 0,
            complexity_functions: Some(0),
            size_functions: Some(0),
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
