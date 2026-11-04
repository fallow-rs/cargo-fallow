//! Complexity per function: cyclomatic and cognitive.
//!
//! The definitions follow the complexity model of fallow for JavaScript, with
//! the Rust forms of each construct.
//!
//! Each `fn` with a body and each closure is one unit. A nested unit has its
//! own counts, and they do not add to the parent.
//!
//! Cyclomatic complexity starts at 1. These add 1 each: `if` and `else if`,
//! `while`, `for`, `loop`, `let ... else`, a `match` arm whose pattern is not
//! `_`, a match guard, `&&` and `||`.
//!
//! Cognitive complexity follows the SonarSource rules:
//!
//! - `if`, `match`, `while`, `for`, `loop` and `let ... else` add 1 plus the
//!   nesting level. Their bodies, the match arms and the `else` block are one
//!   level deeper.
//! - `else if` and `else` add 1 with no nesting part.
//! - A labeled `break` or `continue` adds 1.
//! - A sequence of the same logical operator adds 1. Each change of the
//!   operator adds 1 more.
//!
//! The `?` operator, recursion and code in macro calls add nothing. The parser
//! keeps a macro call as tokens, so its conditions are not visible.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ra_ap_syntax::ast::{self, BinaryOp, ElseBranch, HasLoopBody, HasName};
use ra_ap_syntax::{AstNode, NodeOrToken, SyntaxKind, SyntaxNode};
use serde::Serialize;

use crate::error::CliError;
use crate::finding::{self as shared, Category, Draft, Impact, Location};
use crate::gates::RustGateName as Gate;
use crate::render::Progress;
use crate::scope::Scope;
use crate::section::{Section, to_value};
use crate::structure::ComplexityArgs;
use crate::structure::config::{self, matches_prefix};
use crate::structure::report::{Action, Report, count_gate, gates_text, plural, shared_actions};
use crate::structure::sarif;
use crate::structure::source::{self, LineIndex, Loaded, Needs, Skipped, is_space_or_comment};
use crate::workspace;

/// The rule of each complexity finding.
pub const RULE: &str = "complexity";

/// The comment kind that suppresses a complexity finding.
const SUPPRESS_KIND: &str = "complexity";
/// The suppression comment.
const SUPPRESS_COMMENT: &str = "// fallow-ignore-next-line complexity";
/// The `high` band of cognitive complexity.
const COGNITIVE_HIGH: u32 = 25;
/// The `critical` band of cognitive complexity.
const COGNITIVE_CRITICAL: u32 = 40;
/// The `high` band of cyclomatic complexity.
const CYCLOMATIC_HIGH: u32 = 30;
/// The `critical` band of cyclomatic complexity.
const CYCLOMATIC_CRITICAL: u32 = 50;

/// The name of a closure that no `let` names.
const CLOSURE_NAME: &str = "<closure>";

/// One function or closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// The name: `name`, `Type::name` in an `impl` or a trait, or the `let`
    /// name of a closure.
    pub name: String,
    /// The line of the name. It starts at 1.
    pub line: u32,
    /// The column of the name. It starts at 1.
    pub column: u32,
    /// The first line of the function, without its attributes and its doc
    /// comments.
    pub start_line: u32,
    /// The first line of the attributes and the doc comments.
    pub attr_line: u32,
    /// The last line.
    pub end_line: u32,
    /// The cyclomatic complexity.
    pub cyclomatic: u32,
    /// The cognitive complexity.
    pub cognitive: u32,
    /// The number of parameters, with `self`.
    pub params: u32,
}

impl Function {
    /// The number of lines from the first line to the last line.
    pub const fn line_count(&self) -> u32 {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

/// The functions and closures of a file, sorted by position.
pub fn functions(root: &SyntaxNode, index: &LineIndex<'_>) -> Vec<Function> {
    let mut walker = Walker {
        index,
        owners: Vec::new(),
        found: Vec::new(),
    };
    walker.outside(root);
    walker.found.sort_by_key(|found| (found.line, found.column));
    walker.found
}

/// The counts of the unit that the walk is in.
struct Frame {
    cyclomatic: u32,
    cognitive: u32,
    last_logical: Option<ast::LogicOp>,
}

struct Walker<'a, 'b> {
    index: &'a LineIndex<'b>,
    /// The `impl` and trait names around the walk.
    owners: Vec<String>,
    found: Vec<Function>,
}

impl Walker<'_, '_> {
    /// Walks code outside a unit: items, `impl` blocks and initializers.
    fn outside(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SyntaxKind::FN => self.function(node),
            SyntaxKind::CLOSURE_EXPR => self.closure(node),
            SyntaxKind::IMPL | SyntaxKind::TRAIT => {
                self.owners.push(owner_name(node));
                for child in node.children() {
                    self.outside(&child);
                }
                self.owners.pop();
            }
            _ => {
                for child in node.children() {
                    self.outside(&child);
                }
            }
        }
    }

    fn function(&mut self, node: &SyntaxNode) {
        let Some(item) = ast::Fn::cast(node.clone()) else {
            return;
        };
        let Some(body) = item.body() else {
            return;
        };
        let name = item
            .name()
            .map_or_else(String::new, |name| crate::attrs::plain_name(&name));
        let name = match self.owners.last() {
            Some(owner) if !owner.is_empty() => format!("{owner}::{name}"),
            _ => name,
        };
        let anchor = item.name().map_or_else(
            || node.text_range().start(),
            |name| name.syntax().text_range().start(),
        );
        let params = item.param_list().map_or(0, |list| {
            list.params().count() + usize::from(list.self_param().is_some())
        });
        // An item inside the body is not part of an outer `impl`.
        let owners = std::mem::take(&mut self.owners);
        let frame = self.unit(body.syntax());
        self.owners = owners;
        self.push(node, name, anchor, params, &frame);
    }

    fn closure(&mut self, node: &SyntaxNode) {
        let Some(closure) = ast::ClosureExpr::cast(node.clone()) else {
            return;
        };
        let params = closure.param_list().map_or(0, |list| list.params().count());
        let frame = match closure.body() {
            Some(body) => self.unit(body.syntax()),
            None => Frame::new(),
        };
        let name = closure_name(node);
        self.push(node, name, node.text_range().start(), params, &frame);
    }

    fn unit(&mut self, body: &SyntaxNode) -> Frame {
        let mut frame = Frame::new();
        self.inside(body, &mut frame, 0);
        frame
    }

    fn push(
        &mut self,
        node: &SyntaxNode,
        name: String,
        anchor: ra_ap_syntax::TextSize,
        params: usize,
        frame: &Frame,
    ) {
        let (line, column) = self.index.position(anchor);
        self.found.push(Function {
            name,
            line,
            column,
            start_line: self.index.position(code_start(node)).0,
            attr_line: self.index.position(node.text_range().start()).0,
            end_line: self.index.end_line(node.text_range().end()),
            cyclomatic: frame.cyclomatic,
            cognitive: frame.cognitive,
            params: u32::try_from(params).unwrap_or(u32::MAX),
        });
    }

    /// Walks code inside a unit at the nesting level `nesting`.
    fn inside(&mut self, node: &SyntaxNode, frame: &mut Frame, nesting: u32) {
        match node.kind() {
            SyntaxKind::FN | SyntaxKind::CLOSURE_EXPR | SyntaxKind::IMPL | SyntaxKind::TRAIT => {
                self.outside(node);
            }
            SyntaxKind::IF_EXPR => self.if_expr(node, frame, nesting, false),
            SyntaxKind::WHILE_EXPR | SyntaxKind::FOR_EXPR | SyntaxKind::LOOP_EXPR => {
                frame.cyclomatic += 1;
                frame.cognitive += 1 + nesting;
                let body = loop_body(node);
                for child in node.children() {
                    let level = if Some(&child) == body.as_ref() {
                        nesting + 1
                    } else {
                        nesting
                    };
                    self.inside(&child, frame, level);
                }
            }
            SyntaxKind::MATCH_EXPR => {
                frame.cognitive += 1 + nesting;
                for child in node.children() {
                    if child.kind() == SyntaxKind::MATCH_ARM_LIST {
                        for arm in child.children() {
                            self.match_arm(&arm, frame, nesting + 1);
                        }
                    } else {
                        self.inside(&child, frame, nesting);
                    }
                }
            }
            SyntaxKind::LET_ELSE => {
                frame.cyclomatic += 1;
                frame.cognitive += 1 + nesting;
                self.children(node, frame, nesting + 1);
            }
            SyntaxKind::BIN_EXPR => self.binary(node, frame, nesting),
            SyntaxKind::BREAK_EXPR | SyntaxKind::CONTINUE_EXPR => {
                if node
                    .children()
                    .any(|child| child.kind() == SyntaxKind::LIFETIME)
                {
                    frame.cognitive += 1;
                }
                self.children(node, frame, nesting);
            }
            _ => self.children(node, frame, nesting),
        }
    }

    fn children(&mut self, node: &SyntaxNode, frame: &mut Frame, nesting: u32) {
        for child in node.children() {
            self.inside(&child, frame, nesting);
        }
    }

    fn if_expr(&mut self, node: &SyntaxNode, frame: &mut Frame, nesting: u32, else_if: bool) {
        frame.cyclomatic += 1;
        frame.cognitive += if else_if { 1 } else { 1 + nesting };
        let Some(expr) = ast::IfExpr::cast(node.clone()) else {
            return;
        };
        let then = expr.then_branch().map(|block| block.syntax().clone());
        let other = expr.else_branch();
        for child in node.children() {
            if Some(&child) == then.as_ref() {
                self.inside(&child, frame, nesting + 1);
                continue;
            }
            match &other {
                Some(ElseBranch::IfExpr(next)) if next.syntax() == &child => {
                    self.if_expr(&child, frame, nesting, true);
                }
                Some(ElseBranch::Block(block)) if block.syntax() == &child => {
                    frame.cognitive += 1;
                    self.inside(&child, frame, nesting + 1);
                }
                _ => self.inside(&child, frame, nesting),
            }
        }
    }

    fn match_arm(&mut self, arm: &SyntaxNode, frame: &mut Frame, nesting: u32) {
        let Some(parsed) = ast::MatchArm::cast(arm.clone()) else {
            return;
        };
        if !matches!(parsed.pat(), Some(ast::Pat::WildcardPat(_))) {
            frame.cyclomatic += 1;
        }
        if parsed.guard().is_some() {
            frame.cyclomatic += 1;
        }
        self.children(arm, frame, nesting);
    }

    fn binary(&mut self, node: &SyntaxNode, frame: &mut Frame, nesting: u32) {
        let Some(op) = logical_op(node) else {
            self.children(node, frame, nesting);
            return;
        };
        frame.cyclomatic += 1;
        if frame.last_logical != Some(op) {
            frame.cognitive += 1;
        }
        frame.last_logical = Some(op);
        self.children(node, frame, nesting);
        if !node.children().any(|child| logical_op(&child).is_some()) {
            frame.last_logical = None;
        }
    }
}

impl Frame {
    const fn new() -> Self {
        Self {
            cyclomatic: 1,
            cognitive: 0,
            last_logical: None,
        }
    }
}

fn logical_op(node: &SyntaxNode) -> Option<ast::LogicOp> {
    match ast::BinExpr::cast(node.clone())?.op_kind()? {
        BinaryOp::LogicOp(op) => Some(op),
        _ => None,
    }
}

fn loop_body(node: &SyntaxNode) -> Option<SyntaxNode> {
    let body = match node.kind() {
        SyntaxKind::WHILE_EXPR => ast::WhileExpr::cast(node.clone())?.loop_body(),
        SyntaxKind::FOR_EXPR => ast::ForExpr::cast(node.clone())?.loop_body(),
        SyntaxKind::LOOP_EXPR => ast::LoopExpr::cast(node.clone())?.loop_body(),
        _ => None,
    };
    body.map(|block| block.syntax().clone())
}

/// The name of an `impl` block or a trait: the self type, or the trait name.
fn owner_name(node: &SyntaxNode) -> String {
    let text = if let Some(block) = ast::Impl::cast(node.clone()) {
        block.self_ty().map(|ty| ty.syntax().text().to_string())
    } else {
        ast::Trait::cast(node.clone())
            .and_then(|item| item.name())
            .map(|name| name.text().to_string())
    };
    text.map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

/// The `let` name of a closure, or [`CLOSURE_NAME`].
fn closure_name(node: &SyntaxNode) -> String {
    let named = node
        .parent()
        .and_then(ast::LetStmt::cast)
        .and_then(|stmt| match stmt.pat()? {
            ast::Pat::IdentPat(pat) => pat.name(),
            _ => None,
        });
    named.map_or_else(
        || CLOSURE_NAME.to_owned(),
        |name| crate::attrs::plain_name(&name),
    )
}

/// The start of the code of an item, after its attributes and its comments.
fn code_start(node: &SyntaxNode) -> ra_ap_syntax::TextSize {
    node.children_with_tokens()
        .find(|child| match child {
            NodeOrToken::Node(inner) => {
                !matches!(inner.kind(), SyntaxKind::ATTR | SyntaxKind::DOC_COMMENT)
            }
            NodeOrToken::Token(token) => !is_space_or_comment(token.kind()),
        })
        .map_or_else(
            || node.text_range().start(),
            |child| child.text_range().start(),
        )
}

/// The severity band of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "ComplexitySeverity")]
pub enum Severity {
    /// Over a limit, under the `high` band.
    Moderate,
    /// Cognitive 25 or more, or cyclomatic 30 or more.
    High,
    /// Cognitive 40 or more, or cyclomatic 50 or more.
    Critical,
}

fn severity(cyclomatic: u32, cognitive: u32) -> Severity {
    if cognitive >= COGNITIVE_CRITICAL || cyclomatic >= CYCLOMATIC_CRITICAL {
        Severity::Critical
    } else if cognitive >= COGNITIVE_HIGH || cyclomatic >= CYCLOMATIC_HIGH {
        Severity::High
    } else {
        Severity::Moderate
    }
}

/// The limits that a function went over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Exceeded {
    /// Only the cyclomatic limit.
    Cyclomatic,
    /// Only the cognitive limit.
    Cognitive,
    /// Both limits.
    Both,
}

/// One function over a limit.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "ComplexityFinding")]
pub struct Finding {
    /// The file.
    pub path: String,
    /// The function name.
    pub name: String,
    /// The line of the name.
    pub line: u32,
    /// The column of the name.
    pub col: u32,
    /// The cyclomatic complexity.
    pub cyclomatic: u32,
    /// The cognitive complexity.
    pub cognitive: u32,
    /// The lines of the function.
    pub line_count: u32,
    /// The parameters, with `self`.
    pub param_count: u32,
    /// The limits that the function went over.
    pub exceeded: Exceeded,
    /// The severity band.
    pub severity: Severity,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// The summary of a complexity run.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "ComplexitySummary")]
pub struct Summary {
    /// The parsed files.
    pub files: usize,
    /// The functions and closures.
    pub functions: usize,
    /// The cyclomatic limit.
    pub max_cyclomatic: u32,
    /// The cognitive limit.
    pub max_cognitive: u32,
    /// The findings that a comment suppressed.
    pub suppressed: usize,
    /// The files with a generator header or an `@generated` comment, which
    /// the check skips.
    pub generated_files: usize,
    /// The files that a declaration reached but that did not parse.
    pub skipped_files: Vec<Skipped>,
}

pub fn run(args: &ComplexityArgs) -> Result<Section, CliError> {
    let progress = Progress::new(args.common.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(
        args.common.manifest_path.as_deref(),
        &args.common.cargo.flags(),
    )?;
    let mut settings: config::Complexity = config::section(&workspace, "complexity")?;
    if let Some(value) = args.max_cyclomatic {
        settings.max_cyclomatic = value;
    }
    if let Some(value) = args.max_cognitive {
        settings.max_cognitive = value;
    }
    progress.step("Measuring the complexity");
    let loaded = source::load(
        &workspace,
        Needs {
            functions: true,
            tokens: None,
        },
    );
    // A limit flag changes the findings, so a baseline entry can be stale
    // with no change of the code.
    let scope = Scope::load(&workspace, &args.common.scope)?
        .narrowed(args.max_cyclomatic.is_some() || args.max_cognitive.is_some());
    scope.annotate(section(
        &loaded,
        &settings,
        args.fail_on_complexity,
        &scope,
    )?)
}

/// Measures each function of `loaded`. `enforced` is true when a function over
/// a limit fails the run.
pub fn section(
    loaded: &Loaded,
    settings: &config::Complexity,
    enforced: bool,
    scope: &Scope,
) -> Result<Section, CliError> {
    let mut findings = Vec::new();
    let (mut count, mut suppressed) = (0, 0);
    for file in loaded
        .files
        .iter()
        .filter(|file| !matches_prefix(&file.path, &settings.exclude))
    {
        count += file.functions.len();
        for function in &file.functions {
            let Some(finding) = finding(&file.path, function, settings) else {
                continue;
            };
            if !scope.keeps(&file.path) {
                continue;
            }
            if scope.hides(
                "complexity",
                SUPPRESS_KIND,
                &file.path,
                Some(&(function.attr_line.saturating_sub(1)..=function.line)),
            ) {
                suppressed += 1;
                continue;
            }
            findings.push(finding);
        }
    }
    findings.sort_by(|a, b| {
        (b.severity, b.cognitive.max(b.cyclomatic), &a.path, a.line).cmp(&(
            a.severity,
            a.cognitive.max(a.cyclomatic),
            &b.path,
            b.line,
        ))
    });
    let summary = Summary {
        files: loaded.files.len(),
        functions: count,
        max_cyclomatic: settings.max_cyclomatic,
        max_cognitive: settings.max_cognitive,
        suppressed,
        generated_files: loaded.files.iter().filter(|file| file.skip_metrics).count(),
        skipped_files: loaded.skipped_files(),
    };
    let mut report = Report::new("complexity", summary, findings);
    let mut findings = shared_findings(&report);
    report.retain_baseline(scope, &mut findings);
    report.gate_outcomes.insert(
        Gate::HealthFindings,
        count_gate(
            if scope.rules().counts(&[RULE], true) {
                report.findings.len()
            } else {
                0
            },
            enforced,
        ),
    );
    Ok(Section {
        human: human(&report),
        json: to_value(&report)?,
        sarif: sarif_log(&findings, &report, enforced),
        findings,
        gates: report.gate_outcomes,
    })
}

/// The CodeClimate severity of a band, the same as in fallow.
const fn impact(severity: Severity) -> Impact {
    match severity {
        Severity::Moderate => Impact::Minor,
        Severity::High => Impact::Major,
        Severity::Critical => Impact::Critical,
    }
}

/// The findings in the shared shape. The id uses the file, the function name
/// and its place among the functions of the same name in the file, not the
/// line.
fn shared_findings(report: &Report<Summary, Finding>) -> Vec<shared::Finding> {
    let keys: Vec<((&str, &str), u32)> = report
        .findings
        .iter()
        .map(|finding| ((finding.path.as_str(), finding.name.as_str()), finding.line))
        .collect();
    report
        .findings
        .iter()
        .zip(shared::occurrences(&keys))
        .map(|(finding, index)| {
            let index = index.to_string();
            shared::Finding::new(Draft {
                check: "complexity",
                rule: RULE,
                severity: shared::Severity::Error,
                message: format!(
                    "`{}` has cyclomatic complexity {} and cognitive complexity {}.",
                    finding.name, finding.cyclomatic, finding.cognitive
                ),
                location: Location::at(finding.path.clone(), finding.line, finding.col),
                identity: &[&finding.path, &finding.name, &index],
                category: Category::Complexity,
                impact: impact(finding.severity),
            })
            .with_actions(shared_actions(&finding.actions))
        })
        .collect()
}

fn finding(path: &str, function: &Function, settings: &config::Complexity) -> Option<Finding> {
    let over_cyclomatic = function.cyclomatic > settings.max_cyclomatic;
    let over_cognitive = function.cognitive > settings.max_cognitive;
    let exceeded = match (over_cyclomatic, over_cognitive) {
        (true, true) => Exceeded::Both,
        (true, false) => Exceeded::Cyclomatic,
        (false, true) => Exceeded::Cognitive,
        (false, false) => return None,
    };
    Some(Finding {
        path: path.to_owned(),
        name: function.name.clone(),
        line: function.line,
        col: function.column,
        cyclomatic: function.cyclomatic,
        cognitive: function.cognitive,
        line_count: function.line_count(),
        param_count: function.params,
        exceeded,
        severity: severity(function.cyclomatic, function.cognitive),
        actions: vec![
            Action::manual(
                "refactor-function",
                format!(
                    "Split `{}` into smaller functions (cyclomatic {}, cognitive {})",
                    function.name, function.cyclomatic, function.cognitive
                ),
            ),
            Action::suppress(SUPPRESS_COMMENT, "above-function-declaration"),
        ],
    })
}

fn human(report: &Report<Summary, Finding>) -> String {
    let summary = &report.summary;
    let mut text = format!(
        "cargo fallow complexity: {} {}, {} {}, limits cyclomatic {} and cognitive {}\n",
        summary.files,
        plural(summary.files, "file", "files"),
        summary.functions,
        plural(summary.functions, "function", "functions"),
        summary.max_cyclomatic,
        summary.max_cognitive,
    );
    if report.findings.is_empty() {
        text.push_str("No function is over a limit\n");
    }
    for finding in &report.findings {
        let _ = writeln!(
            text,
            "  {}:{} {} cyclomatic {} cognitive {} ({}, {} lines)",
            finding.path,
            finding.line,
            finding.name,
            finding.cyclomatic,
            finding.cognitive,
            severity_label(finding.severity),
            finding.line_count,
        );
    }
    for file in &summary.skipped_files {
        let _ = writeln!(text, "  skipped {}: {}", file.path, file.reason);
    }
    text.push_str(&gates_text(&report.gate_outcomes));
    text
}

const fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Moderate => "moderate",
        Severity::High => "high",
        Severity::Critical => "critical",
    }
}

fn sarif_log(
    findings: &[shared::Finding],
    report: &Report<Summary, Finding>,
    enforced: bool,
) -> serde_json::Value {
    let rules = BTreeMap::from([(
        "complexity".to_owned(),
        "A function is over the cyclomatic or the cognitive complexity limit.".to_owned(),
    )]);
    sarif::log(
        &rules,
        findings,
        sarif::level(enforced),
        &report.gate_outcomes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{Edition, with_parsed};

    fn measure(source: &str) -> Vec<(String, u32, u32)> {
        with_parsed(source, Edition::Edition2024, |file, text| {
            functions(file.syntax(), &LineIndex::new(text))
                .into_iter()
                .map(|f| (f.name, f.cyclomatic, f.cognitive))
                .collect()
        })
        .unwrap_or_default()
    }

    fn one(source: &str) -> (u32, u32) {
        let found = measure(source);
        assert_eq!(found.len(), 1, "{found:?}");
        (found[0].1, found[0].2)
    }

    /// The walk recurses once for each tree level on the parse stack. These
    /// shapes give a tree near the depth limit of `crate::parse` (2600 levels
    /// in a debug build on Windows) with a small scan depth for each tree level.
    #[test]
    fn the_walk_of_a_deep_tree_fits_on_the_parse_stack() {
        let chain = format!("fn f() -> u32 {{ 1{} }}", " + 1".repeat(2500));
        assert_eq!(one(&chain).0, 1);
        let branches = format!(
            "fn f(a: bool) {{ if a {{}}{} }}",
            " else if a {}".repeat(1400)
        );
        assert_eq!(one(&branches).0, 1402);
    }

    #[test]
    fn a_straight_function_has_cyclomatic_one_and_cognitive_zero() {
        assert_eq!(one("fn f(a: u8) -> u8 { a + 1 }"), (1, 0));
    }

    #[test]
    fn if_else_if_else_follow_the_flat_increments() {
        // if +1, else if +1 flat, else +1 flat.
        let source = "fn f(a: u8) -> u8 { if a == 0 { 1 } else if a == 1 { 2 } else { 3 } }";
        assert_eq!(one(source), (3, 3));
    }

    #[test]
    fn nesting_adds_the_level_to_each_structure() {
        // for +1 (level 0), if +2 (level 1), while +3 (level 2).
        let source = "fn f(v: &[u8]) { for x in v { if *x > 0 { while true {} } } }";
        assert_eq!(one(source), (4, 6));
    }

    #[test]
    fn an_else_if_body_stays_at_the_level_of_the_first_body() {
        // if +1, else if +1, and the inner if in the else-if body at level 1: +2.
        let source = "fn f(a: u8, b: bool) { if a == 0 {} else if a == 1 { if b {} } }";
        assert_eq!(one(source), (4, 4));
    }

    #[test]
    fn match_counts_once_for_cognitive_and_each_arm_but_the_wildcard_for_cyclomatic() {
        let source = "fn f(a: u8) -> u8 { match a { 0 => 1, 1 if true => 2, _ => 3 } }";
        // arms 0 and 1 (+2), guard (+1); cognitive: match +1.
        assert_eq!(one(source), (4, 1));
    }

    #[test]
    fn logical_sequences_add_one_per_change_of_operator() {
        assert_eq!(
            one("fn f(a: bool, b: bool, c: bool) -> bool { a && b && c }"),
            (3, 1)
        );
        assert_eq!(
            one("fn f(a: bool, b: bool, c: bool) -> bool { a && b || c }"),
            (3, 2)
        );
        assert_eq!(
            one("fn f(a: bool, b: bool, c: bool, d: bool) -> bool { (a && b) || (c && d) }"),
            (4, 3)
        );
    }

    #[test]
    fn the_known_differences_in_the_readme_hold() {
        // The operator memory passes into a group in parentheses.
        assert_eq!(
            one("fn f(a: bool, b: bool, c: bool) -> bool { a && (b && c) }"),
            (3, 1)
        );
        assert_eq!(
            one("fn f(a: bool, b: bool, c: bool) -> bool { a && (b || c) }"),
            (3, 2)
        );
        // Code in a macro call adds nothing.
        assert_eq!(one("fn f(a: bool, b: bool) { assert!(a && b); }"), (1, 0));
        // `loop` counts the same as `while`.
        assert_eq!(one("fn f() { loop {} }"), (2, 1));
        // `?` adds nothing.
        assert_eq!(
            one("fn f(a: Option<u8>) -> Option<u8> { let b = a?; Some(b) }"),
            (1, 0)
        );
    }

    #[test]
    fn labeled_jumps_and_let_else_count() {
        let source = "fn f(v: Option<u8>) { 'outer: loop { let Some(x) = v else { break 'outer; }; continue; } }";
        // loop +1 (0), let-else +2 (level 1), labeled break +1; cyclomatic 1 + loop + let-else.
        assert_eq!(one(source), (3, 4));
    }

    #[test]
    fn closures_and_nested_functions_are_separate_units() {
        let source = "impl S { fn f(&self, v: Vec<u8>) -> usize { let keep = |x: &u8| *x > 0 && *x < 9; fn inner(a: bool) -> bool { if a { true } else { false } } v.iter().filter(|x| keep(x)).count() } }";
        let mut found = measure(source);
        found.sort();
        assert_eq!(
            found,
            vec![
                ("<closure>".to_owned(), 1, 0),
                ("S::f".to_owned(), 1, 0),
                ("inner".to_owned(), 2, 2),
                ("keep".to_owned(), 2, 1),
            ]
        );
    }

    #[test]
    fn the_question_mark_operator_adds_nothing() {
        assert_eq!(
            one("fn f(a: Option<u8>) -> Option<u8> { let b = a?; Some(b) }"),
            (1, 0)
        );
    }

    #[test]
    fn a_finding_needs_a_value_over_a_limit_and_bands_follow_the_highest_metric() {
        let settings = config::Complexity::default();
        let function = Function {
            name: "f".to_owned(),
            line: 1,
            column: 4,
            start_line: 1,
            attr_line: 1,
            end_line: 3,
            cyclomatic: 20,
            cognitive: 15,
            params: 0,
        };
        assert!(finding("a.rs", &function, &settings).is_none());
        let over = Function {
            cognitive: 16,
            ..function
        };
        let found = finding("a.rs", &over, &settings);
        assert_eq!(
            found.as_ref().map(|f| f.exceeded),
            Some(Exceeded::Cognitive)
        );
        assert_eq!(found.map(|f| f.severity), Some(Severity::Moderate));
        assert_eq!(severity(30, 0), Severity::High);
        assert_eq!(severity(0, 40), Severity::Critical);
    }
}
