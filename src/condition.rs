//! MSBuild condition parser and evaluator.
//!
//! Parses and evaluates conditions found in `.dproj` `<PropertyGroup>` and
//! `<Import>` elements, for example:
//!
//! - `'$(Config)'=='Debug' And '$(Platform)'=='Win32'`
//! - `('$(Platform)'=='Win32' and '$(Base)'=='true') or '$(Base_Win32)'!=''`
//! - `Exists('$(BDS)\Bin\CodeGear.Delphi.Targets')`
//!
//! Uses [`chumsky`] for the parsing grammar.
//!
//! ## Grammar (case-insensitive keywords)
//!
//! ```text
//! expr       = or_expr
//! or_expr    = and_expr ('or' and_expr)*
//! and_expr   = atom ('and' atom)*
//! atom       = comparison | exists | '(' expr ')'
//! comparison = quoted ('==' | '!=') quoted
//! exists     = 'Exists' '(' quoted ')'
//! quoted     = "'" chars "'"
//! ```

#![allow(dead_code)]

use chumsky::prelude::*;
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════════
//  AST
// ═══════════════════════════════════════════════════════════════════════════════

/// A parsed MSBuild condition expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    /// `'lhs' == 'rhs'` or `'lhs' != 'rhs'`.
    Compare {
        lhs: Vec<ExprValue>,
        op: CompareOp,
        rhs: Vec<ExprValue>,
    },
    /// `Exists('path')` — always evaluates to `true` (no filesystem checks).
    Exists(Vec<ExprValue>),
    /// `a and b` (case-insensitive keyword).
    And(Box<Expression>, Box<Expression>),
    /// `a or b` (case-insensitive keyword).
    Or(Box<Expression>, Box<Expression>),
}

/// Comparison operator used inside a [`CondExpr::Compare`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
}

/// A fragment of a string value that may contain `$(Variable)` references.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprValue {
    /// Literal text (no variable expansion needed).
    Literal(String),
    /// A `$(VarName)` reference that will be expanded during evaluation.
    Variable(String),
}

// ═══════════════════════════════════════════════════════════════════════════════
//  String-part splitting
// ═══════════════════════════════════════════════════════════════════════════════

/// Split the raw text between single quotes into [`StringPart`] fragments.
///
/// `$(VarName)` sequences become [`StringPart::Variable`]; everything else
/// becomes [`StringPart::Literal`].
fn parse_string_parts(s: &str) -> Vec<ExprValue> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'(') {
            if !literal.is_empty() {
                parts.push(ExprValue::Literal(std::mem::take(&mut literal)));
            }
            chars.next(); // consume '('
            let var_name: String = chars.by_ref().take_while(|&ch| ch != ')').collect();
            parts.push(ExprValue::Variable(var_name));
        } else {
            literal.push(c);
        }
    }

    if !literal.is_empty() {
        parts.push(ExprValue::Literal(literal));
    }

    parts
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Chumsky parser
// ═══════════════════════════════════════════════════════════════════════════════

/// Build the chumsky parser for MSBuild condition expressions.
fn condition_parser<'a>() -> impl Parser<'a, &'a str, Expression, extra::Err<Simple<'a, char>>> {
    recursive(|expr| {
        // ── Single-quoted string value ───────────────────────────────────
        let quoted = just('\'')
            .ignore_then(none_of('\'').repeated().to_slice())
            .then_ignore(just('\''))
            .map(parse_string_parts);

        // ── Comparison operators ─────────────────────────────────────────
        let cmp_op = just("==")
            .to(CompareOp::Equal)
            .or(just("!=").to(CompareOp::NotEqual));

        // ── Comparison:  'lhs' op 'rhs' ─────────────────────────────────
        let comparison = quoted
            .padded()
            .then(cmp_op.padded())
            .then(quoted.padded())
            .map(|((lhs, op), rhs)| Expression::Compare { lhs, op, rhs });

        // ── Case-insensitive alphabetic word (for keyword matching) ──────
        let alpha_word = any()
            .filter(|c: &char| c.is_ascii_alphabetic())
            .repeated()
            .at_least(1)
            .to_slice();

        // ── Exists('path') ───────────────────────────────────────────────
        let exists = alpha_word
            .filter(|s: &&str| s.eq_ignore_ascii_case("exists"))
            .ignore_then(just('(').padded())
            .ignore_then(quoted)
            .then_ignore(just(')').padded())
            .map(Expression::Exists);

        // ── Parenthesized expression ─────────────────────────────────────
        let paren_expr = expr.delimited_by(just('(').padded(), just(')').padded());

        // ── Atom ─────────────────────────────────────────────────────────
        let atom = choice((comparison, exists, paren_expr)).padded();

        // ── 'and' — higher precedence than 'or' ─────────────────────────
        let and_kw = alpha_word
            .filter(|s: &&str| s.eq_ignore_ascii_case("and"))
            .padded();

        let and_expr = atom.clone().foldl(
            and_kw.ignore_then(atom).repeated(),
            |lhs, rhs| Expression::And(Box::new(lhs), Box::new(rhs)),
        );

        // ── 'or' — lowest precedence ────────────────────────────────────
        let or_kw = alpha_word
            .filter(|s: &&str| s.eq_ignore_ascii_case("or"))
            .padded();

        and_expr.clone().foldl(
            or_kw.ignore_then(and_expr).repeated(),
            |lhs, rhs| Expression::Or(Box::new(lhs), Box::new(rhs)),
        )
    })
}

/// Parse a condition attribute string into a [`CondExpr`] AST.
pub fn parse_condition(input: &str) -> Result<Expression, String> {
    condition_parser()
        .parse(input)
        .into_result()
        .map_err(|errs| {
            let messages: Vec<String> = errs.iter().map(|e| format!("{e}")).collect();
            format!(
                "Failed to parse condition '{}': {}",
                input,
                messages.join("; ")
            )
        })
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Evaluation
// ═══════════════════════════════════════════════════════════════════════════════

/// Expand `$(Var)` references in a parsed string expression.
/// Unknown variables expand to the empty string.
fn expand_string(parts: &[ExprValue], vars: &HashMap<String, String>) -> String {
    parts
        .iter()
        .map(|part| match part {
            ExprValue::Literal(s) => s.clone(),
            ExprValue::Variable(name) => vars.get(name.as_str()).cloned().unwrap_or_default(),
        })
        .collect()
}

/// Evaluate a condition expression against a set of variable bindings.
///
/// `Exists(…)` always evaluates to `true` — filesystem checks are not
/// performed.
pub fn evaluate(expr: &Expression, vars: &HashMap<String, String>) -> bool {
    match expr {
        Expression::Compare { lhs, op, rhs } => {
            let l = expand_string(lhs, vars);
            let r = expand_string(rhs, vars);
            match op {
                CompareOp::Equal => l == r,
                CompareOp::NotEqual => l != r,
            }
        }
        Expression::Exists(_) => true,
        Expression::And(a, b) => evaluate(a, vars) && evaluate(b, vars),
        Expression::Or(a, b) => evaluate(a, vars) || evaluate(b, vars),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── String-part splitting ────────────────────────────────────────────

    #[test]
    fn string_parts_literal_only() {
        assert_eq!(
            parse_string_parts("hello"),
            vec![ExprValue::Literal("hello".into())]
        );
    }

    #[test]
    fn string_parts_variable_only() {
        assert_eq!(
            parse_string_parts("$(Config)"),
            vec![ExprValue::Variable("Config".into())]
        );
    }

    #[test]
    fn string_parts_mixed() {
        assert_eq!(
            parse_string_parts("$(APPDATA)\\Embarcadero\\$(BDSAPPDATABASEDIR)"),
            vec![
                ExprValue::Variable("APPDATA".into()),
                ExprValue::Literal("\\Embarcadero\\".into()),
                ExprValue::Variable("BDSAPPDATABASEDIR".into()),
            ]
        );
    }

    #[test]
    fn string_parts_empty() {
        assert_eq!(parse_string_parts(""), Vec::<ExprValue>::new());
    }

    // ── Condition parsing ────────────────────────────────────────────────

    #[test]
    fn parse_simple_equality() {
        let expr = parse_condition("'$(Config)'=='Base'").unwrap();
        assert_eq!(
            expr,
            Expression::Compare {
                lhs: vec![ExprValue::Variable("Config".into())],
                op: CompareOp::Equal,
                rhs: vec![ExprValue::Literal("Base".into())],
            }
        );
    }

    #[test]
    fn parse_empty_rhs() {
        let expr = parse_condition("'$(Config)'==''").unwrap();
        assert_eq!(
            expr,
            Expression::Compare {
                lhs: vec![ExprValue::Variable("Config".into())],
                op: CompareOp::Equal,
                rhs: vec![],
            }
        );
    }

    #[test]
    fn parse_inequality() {
        let expr = parse_condition("'$(Base)'!=''").unwrap();
        assert_eq!(
            expr,
            Expression::Compare {
                lhs: vec![ExprValue::Variable("Base".into())],
                op: CompareOp::NotEqual,
                rhs: vec![],
            }
        );
    }

    #[test]
    fn parse_spaced_operators() {
        // Some dproj files use spaces around operators:
        // Condition=" '$(Configuration)' == '' "
        let expr = parse_condition(" '$(Configuration)' == '' ").unwrap();
        assert_eq!(
            expr,
            Expression::Compare {
                lhs: vec![ExprValue::Variable("Configuration".into())],
                op: CompareOp::Equal,
                rhs: vec![],
            }
        );
    }

    #[test]
    fn parse_or_expression() {
        let expr =
            parse_condition("'$(Config)'=='Base' or '$(Base)'!=''").unwrap();
        match &expr {
            Expression::Or(lhs, rhs) => {
                assert!(matches!(lhs.as_ref(), Expression::Compare { op: CompareOp::Equal, .. }));
                assert!(matches!(rhs.as_ref(), Expression::Compare { op: CompareOp::NotEqual, .. }));
            }
            other => panic!("expected Or, got {other:?}"),
        }
    }

    #[test]
    fn parse_and_title_case() {
        // Deployment conditions use title-case "And"
        let expr =
            parse_condition("'$(Config)'=='Debug' And '$(Platform)'=='Win32'")
                .unwrap();
        assert!(matches!(expr, Expression::And(_, _)));
    }

    #[test]
    fn parse_parenthesized_and_or() {
        let input = "('$(Platform)'=='Win32' and '$(Base)'=='true') or '$(Base_Win32)'!=''";
        let expr = parse_condition(input).unwrap();
        match &expr {
            Expression::Or(lhs, _rhs) => {
                assert!(matches!(lhs.as_ref(), Expression::And(_, _)));
            }
            other => panic!("expected Or(And(..), ..), got {other:?}"),
        }
    }

    #[test]
    fn parse_exists() {
        let expr =
            parse_condition("Exists('$(BDS)\\Bin\\CodeGear.Delphi.Targets')")
                .unwrap();
        match &expr {
            Expression::Exists(parts) => {
                assert_eq!(parts[0], ExprValue::Variable("BDS".into()));
                assert_eq!(
                    parts[1],
                    ExprValue::Literal("\\Bin\\CodeGear.Delphi.Targets".into())
                );
            }
            other => panic!("expected Exists, got {other:?}"),
        }
    }

    // ── Evaluation ───────────────────────────────────────────────────────

    fn make_vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn eval_simple_eq_true() {
        let expr = parse_condition("'$(Config)'=='Debug'").unwrap();
        let vars = make_vars(&[("Config", "Debug")]);
        assert!(evaluate(&expr, &vars));
    }

    #[test]
    fn eval_simple_eq_false() {
        let expr = parse_condition("'$(Config)'=='Release'").unwrap();
        let vars = make_vars(&[("Config", "Debug")]);
        assert!(!evaluate(&expr, &vars));
    }

    #[test]
    fn eval_ne_empty_with_value() {
        let expr = parse_condition("'$(Base)'!=''").unwrap();
        let vars = make_vars(&[("Base", "true")]);
        assert!(evaluate(&expr, &vars));
    }

    #[test]
    fn eval_ne_empty_without_value() {
        let expr = parse_condition("'$(Base)'!=''").unwrap();
        let vars = HashMap::new();
        assert!(!evaluate(&expr, &vars));
    }

    #[test]
    fn eval_or_first_true() {
        let expr =
            parse_condition("'$(Config)'=='Debug' or '$(Cfg_1)'!=''").unwrap();
        let vars = make_vars(&[("Config", "Debug")]);
        assert!(evaluate(&expr, &vars));
    }

    #[test]
    fn eval_or_second_true() {
        let expr =
            parse_condition("'$(Config)'=='Debug' or '$(Cfg_1)'!=''").unwrap();
        let vars = make_vars(&[("Config", "Base"), ("Cfg_1", "true")]);
        assert!(evaluate(&expr, &vars));
    }

    #[test]
    fn eval_and_both_true() {
        let expr = parse_condition(
            "'$(Config)'=='Debug' And '$(Platform)'=='Win32'",
        )
        .unwrap();
        let vars = make_vars(&[("Config", "Debug"), ("Platform", "Win32")]);
        assert!(evaluate(&expr, &vars));
    }

    #[test]
    fn eval_and_one_false() {
        let expr = parse_condition(
            "'$(Config)'=='Debug' And '$(Platform)'=='Win32'",
        )
        .unwrap();
        let vars = make_vars(&[("Config", "Debug"), ("Platform", "Win64")]);
        assert!(!evaluate(&expr, &vars));
    }

    #[test]
    fn eval_exists_always_true() {
        let expr =
            parse_condition("Exists('$(BDS)\\Bin\\CodeGear.Delphi.Targets')")
                .unwrap();
        assert!(evaluate(&expr, &HashMap::new()));
    }

    #[test]
    fn eval_compound_and_or() {
        // ('$(Platform)'=='Win32' and '$(Base)'=='true') or '$(Base_Win32)'!=''
        let input = "('$(Platform)'=='Win32' and '$(Base)'=='true') or '$(Base_Win32)'!=''";
        let expr = parse_condition(input).unwrap();

        // Match via the AND branch
        let vars = make_vars(&[("Platform", "Win32"), ("Base", "true")]);
        assert!(evaluate(&expr, &vars));

        // Match via the OR branch (shortcut flag)
        let vars = make_vars(&[("Base_Win32", "true")]);
        assert!(evaluate(&expr, &vars));

        // Neither matches
        let vars = make_vars(&[("Platform", "Win64"), ("Base", "true")]);
        assert!(!evaluate(&expr, &vars));
    }

    // ── Parse every real condition from our dproj files ──────────────────

    #[test]
    fn parse_all_real_conditions() {
        let conditions = [
            "'$(Config)'==''",
            "'$(Platform)'==''",
            "'$(ProjectName)'==''",
            " '$(Configuration)' == '' ",
            "'$(Base)'!=''",
            "'$(Base_Win32)'!=''",
            "'$(Base_Win64)'!=''",
            "'$(Cfg_1)'!=''",
            "'$(Cfg_1_Win32)'!=''",
            "'$(Cfg_1_Win64)'!=''",
            "'$(Cfg_2)'!=''",
            "'$(Cfg_2_Win32)'!=''",
            "'$(Cfg_2_Win64)'!=''",
            "'$(Cfg_3)'!=''",
            "'$(Config)'=='Base' or '$(Base)'!=''",
            "'$(Config)'=='Debug' or '$(Cfg_1)'!=''",
            "'$(Config)'=='Debug' or '$(Cfg_2)'!=''",
            "'$(Config)'=='Release' or '$(Cfg_1)'!=''",
            "'$(Config)'=='Release' or '$(Cfg_2)'!=''",
            "'$(Config)'=='Release_beas' or '$(Cfg_3)'!=''",
            "('$(Platform)'=='Win32' and '$(Base)'=='true') or '$(Base_Win32)'!=''",
            "('$(Platform)'=='Win64' and '$(Base)'=='true') or '$(Base_Win64)'!=''",
            "('$(Platform)'=='Win32' and '$(Cfg_1)'=='true') or '$(Cfg_1_Win32)'!=''",
            "('$(Platform)'=='Win64' and '$(Cfg_1)'=='true') or '$(Cfg_1_Win64)'!=''",
            "('$(Platform)'=='Win32' and '$(Cfg_2)'=='true') or '$(Cfg_2_Win32)'!=''",
            "('$(Platform)'=='Win64' and '$(Cfg_2)'=='true') or '$(Cfg_2_Win64)'!=''",
            "'$(Config)'=='Debug' And '$(Platform)'=='Win32'",
            "'$(Config)'=='Debug' And '$(Platform)'=='Win64'",
            "'$(Config)'=='Release' And '$(Platform)'=='Win32'",
            "'$(Config)'=='Release' And '$(Platform)'=='Win64'",
            "'$(Config)'=='Release_beas' And '$(Platform)'=='Win32'",
            "'$(Config)'=='Release_beas' And '$(Platform)'=='Win64'",
            "Exists('$(BDS)\\Bin\\CodeGear.Delphi.Targets')",
            "Exists('$(APPDATA)\\Embarcadero\\$(BDSAPPDATABASEDIR)\\$(PRODUCTVERSION)\\UserTools.proj')",
            "Exists('$(MSBuildProjectName).deployproj')",
        ];

        for cond in &conditions {
            let result = parse_condition(cond);
            assert!(
                result.is_ok(),
                "Failed to parse condition: {cond}\n  Error: {}",
                result.unwrap_err()
            );
        }
    }
}
