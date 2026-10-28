//! Stratification, Tarjan SCC condensation, and rule safety/aggregate validation.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ast::{Clause, CmpOp, Expr, Lit};
use crate::eval::Engine;
use crate::intern::Term;

#[derive(Debug)]
pub struct StratError(pub String);
impl std::fmt::Display for StratError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stratification error: {}", self.0)
    }
}
impl std::error::Error for StratError {}

impl Engine {
    /// Stratum assignment: depth(p) = max over rules defining p of
    ///   depth(q)        for positive dependencies, and
    ///   depth(q) + 1    for negated dependencies.
    /// Rejects programs where a negative edge lies on a dependency cycle.
    pub fn strata(&self) -> Result<Vec<Vec<usize>>, StratError> {
        let mut defines: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, c) in self.clauses.iter().enumerate() {
            if !c.is_fact {
                defines.entry(c.head.pred.as_str()).or_default().push(i);
            }
        }
        // dependency graph over IDB predicates (head -> body dep)
        let mut deps: HashMap<&str, Vec<(&str, bool)>> = HashMap::new();
        for c in &self.clauses {
            if c.is_fact {
                continue;
            }
            for lit in &c.body {
                let (neg, dep) = match lit {
                    Lit::Neg(a) => (true, a.pred.as_str()),
                    Lit::Pos(a) => (false, a.pred.as_str()),
                    Lit::Cmp(..) | Lit::Now(_) => continue,
                };
                if defines.contains_key(dep) {
                    deps.entry(c.head.pred.as_str())
                        .or_default()
                        .push((dep, neg));
                }
            }
        }
        // negation-cycle check: for every negated edge h -> b, b must not
        // reach h through the dependency graph
        for (h, edges) in &deps {
            for (b, neg) in edges {
                if *neg && reaches(&deps, b, h) {
                    return Err(StratError(format!(
                        "unstratifiable: negation of {b} in a dependency cycle with {h}"
                    )));
                }
            }
        }
        // aggregates must not be read by their own bodies (recursively):
        // folding requires the body to be complete first
        let agg_heads: BTreeSet<&str> = self
            .clauses
            .iter()
            .filter(|c| !c.is_fact && Self::is_agg_clause(c))
            .map(|c| c.head.pred.as_str())
            .collect();
        for c in &self.clauses {
            if c.is_fact || !Self::is_agg_clause(c) {
                continue;
            }
            for lit in &c.body {
                if let Lit::Pos(a) | Lit::Neg(a) = lit {
                    if a.pred == c.head.pred || reaches(&deps, &a.pred, &c.head.pred) {
                        return Err(StratError(format!(
                            "aggregation over recursive dependency: {} reads {}",
                            c.head.pred, a.pred
                        )));
                    }
                }
            }
        }
        // mixed definition: a predicate defined by aggregation clauses may
        // not also have ordinary defining clauses (fold/merge semantics
        // would collide)
        for c in &self.clauses {
            if c.is_fact {
                continue;
            }
            if !Self::is_agg_clause(c) && agg_heads.contains(c.head.pred.as_str()) {
                return Err(StratError(format!(
                    "mixed definition: {} is aggregated in one clause and ordinary in another",
                    c.head.pred
                )));
            }
        }
        // stratification by SCC condensation: predicates in the same
        // strongly-connected component (mutual/self recursion) share a
        // stratum; every cross-SCC dependency is strictly ordered, so a
        // scoped recompute can clear and rebuild level by level with
        // same-pass reader propagation (no extra full-rebuild rounds)
        let idb_list: Vec<&str> = defines.keys().copied().collect();
        let index_of: HashMap<&str, usize> =
            idb_list.iter().enumerate().map(|(i, p)| (*p, i)).collect();
        let n = idb_list.len();
        // Tarjan SCC over head -> dep edges
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (h, edges) in &deps {
            if let Some(&hi) = index_of.get(*h) {
                for (d, _) in edges {
                    if let Some(&di) = index_of.get(*d) {
                        adj[hi].push(di);
                    }
                }
            }
        }
        let mut scc_of: Vec<usize> = vec![usize::MAX; n];
        let mut low = vec![0usize; n];
        let mut disc = vec![0usize; n];
        let mut on_stack = vec![false; n];
        let mut stack: Vec<usize> = Vec::new();
        let mut next_disc = 1usize;
        let mut scc_count = 0usize;
        for start_node in 0..n {
            if disc[start_node] != 0 {
                continue;
            }
            // iterative Tarjan
            let mut call: Vec<(usize, usize)> = vec![(start_node, 0)];
            while let Some((v, ai)) = call.pop() {
                if ai == 0 {
                    disc[v] = next_disc;
                    low[v] = next_disc;
                    next_disc += 1;
                    stack.push(v);
                    on_stack[v] = true;
                }
                let mut recursed = false;
                for (i, &w) in adj[v].iter().enumerate().skip(ai) {
                    if disc[w] == 0 {
                        call.push((v, i + 1));
                        call.push((w, 0));
                        recursed = true;
                        break;
                    } else if on_stack[w] {
                        low[v] = low[v].min(disc[w]);
                    }
                }
                if recursed {
                    continue;
                }
                if low[v] == disc[v] {
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack[w] = false;
                        scc_of[w] = scc_count;
                        if w == v {
                            break;
                        }
                    }
                    scc_count += 1;
                }
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
        // edges between SCCs: level[scc_head] >= level[scc_dep] + 1
        // iterate to fixpoint over condensation edges (small graphs)
        let mut scc_level = vec![0u32; scc_count];
        let mut cond_edges: Vec<(usize, usize)> = Vec::new();
        for (h, edges) in &deps {
            if let Some(&hi) = index_of.get(*h) {
                for (d, _) in edges {
                    if let Some(&di) = index_of.get(*d) {
                        if scc_of[hi] != scc_of[di] {
                            cond_edges.push((scc_of[hi], scc_of[di]));
                        }
                    }
                }
            }
        }
        loop {
            let mut changed = false;
            for (hs, ds) in &cond_edges {
                if scc_level[*hs] < scc_level[*ds] + 1 {
                    scc_level[*hs] = scc_level[*ds] + 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut depth: HashMap<String, u32> = HashMap::new();
        for (i, p) in idb_list.iter().enumerate() {
            depth.insert(p.to_string(), scc_level[scc_of[i]]);
        }

        let mut strata: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, c) in self.clauses.iter().enumerate() {
            if c.is_fact {
                continue;
            }
            let d = depth[&c.head.pred];
            strata.entry(d).or_default().push(i);
        }
        Ok(strata.into_values().collect())
    }

    pub(crate) fn validate_aggregates(
        &self,
        clauses: &[Clause],
    ) -> Result<(), Box<dyn std::error::Error>> {
        fn term_has_aggregate(term: &Term) -> bool {
            matches!(term, Term::Agg(..))
        }

        for clause in clauses {
            if clause.is_fact {
                continue;
            }

            let has_aggregate = clause
                .head
                .args
                .iter()
                .any(|term| matches!(term, Term::Agg(..)));
            let mut seen_aggregate = false;
            for term in &clause.head.args {
                if matches!(term, Term::Agg(..)) {
                    seen_aggregate = true;
                } else if seen_aggregate {
                    return Err(format!(
                        "aggregate columns must be trailing in rule head {:?}",
                        clause.head.pred
                    )
                    .into());
                }
            }
            let mut bound = BTreeSet::new();
            let mut aggregate_in_body = false;

            for literal in &clause.body {
                match literal {
                    Lit::Pos(atom) | Lit::Neg(atom) => {
                        for term in &atom.args {
                            if term_has_aggregate(term) {
                                aggregate_in_body = true;
                            }
                            if let Term::Var(variable) = term {
                                if matches!(literal, Lit::Pos(_)) {
                                    bound.insert(variable.as_str());
                                }
                            }
                        }
                    }
                    Lit::Cmp(_, term, expression) => {
                        if term_has_aggregate(term) {
                            aggregate_in_body = true;
                        }
                        check_expression(expression, &mut |term| {
                            if term_has_aggregate(term) {
                                aggregate_in_body = true;
                            }
                        });
                    }
                    Lit::Now(term) => {
                        if term_has_aggregate(term) {
                            aggregate_in_body = true;
                        }
                    }
                }
            }

            if aggregate_in_body {
                return Err(format!(
                    "aggregates are only allowed in rule heads: {:?}",
                    clause.head.pred
                )
                .into());
            }
            if !has_aggregate {
                continue;
            }

            let mut head_variables = Vec::new();
            for term in &clause.head.args {
                match term {
                    Term::Var(variable) => head_variables.push(variable.as_str()),
                    Term::Agg(_, inner) => {
                        if let Term::Var(variable) = &**inner {
                            head_variables.push(variable.as_str());
                        }
                    }
                    _ => {}
                }
            }
            let unbound: Vec<_> = head_variables
                .iter()
                .filter(|variable| !bound.contains(*variable))
                .copied()
                .collect();
            if !unbound.is_empty() {
                return Err(format!(
                    "unsafe aggregation rule {:?}: head variables {unbound:?} not bound by a positive body atom",
                    clause.head.pred
                )
                .into());
            }
        }
        Ok(())
    }

    pub(crate) fn validate_rule_safety(
        &self,
        clauses: &[Clause],
    ) -> Result<(), Box<dyn std::error::Error>> {
        for clause in clauses {
            if clause.is_fact {
                continue;
            }

            let owner = clause.name.as_deref().unwrap_or(&clause.head.pred);
            let mut bound = BTreeSet::new();
            for literal in &clause.body {
                match literal {
                    Lit::Pos(atom) => {
                        for term in &atom.args {
                            collect_term_variables(term, &mut bound);
                        }
                    }
                    Lit::Neg(atom) => {
                        let mut used = BTreeSet::new();
                        for term in &atom.args {
                            collect_term_variables(term, &mut used);
                        }
                        reject_unbound(owner, "negation", &used, &bound)?;
                    }
                    Lit::Now(Term::Var(variable)) => {
                        bound.insert(variable.clone());
                    }
                    Lit::Now(_) => {}
                    Lit::Cmp(operator, left, expression) => {
                        let mut used = BTreeSet::new();
                        collect_term_variables(left, &mut used);
                        collect_expression_variables(expression, &mut used);

                        if let Term::Var(variable) = left {
                            if *operator == CmpOp::Eq
                                && !bound.contains(variable)
                                && expression_can_resolve(expression, &bound)
                            {
                                bound.insert(variable.clone());
                            }
                        }

                        if term_can_resolve(left, &bound) && *operator == CmpOp::Eq {
                            if let Some((1, Some(variable))) =
                                linear_expression_binding(expression, &bound)
                            {
                                bound.insert(variable);
                            }
                        }

                        reject_unbound(owner, "comparison", &used, &bound)?;
                    }
                }
            }

            let mut head_variables = BTreeSet::new();
            for term in &clause.head.args {
                collect_term_variables(term, &mut head_variables);
            }
            reject_unbound(owner, "head", &head_variables, &bound)?;
        }
        Ok(())
    }
}

/// DFS reachability over the IDB dependency graph.
fn reaches(deps: &HashMap<&str, Vec<(&str, bool)>>, from: &str, to: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(n) = stack.pop() {
        if n == to {
            return true;
        }
        if !seen.insert(n) {
            continue;
        }
        if let Some(edges) = deps.get(n) {
            for (next, _) in edges {
                stack.push(next);
            }
        }
    }
    false
}

pub(crate) fn check_expression<F: FnMut(&Term)>(expression: &Expr, visit: &mut F) {
    match expression {
        Expr::T(term) => visit(term),
        Expr::Add(left, right) | Expr::Sub(left, right) => {
            check_expression(left, visit);
            check_expression(right, visit);
        }
    }
}

fn collect_term_variables(term: &Term, variables: &mut BTreeSet<String>) {
    match term {
        Term::Var(variable) => {
            variables.insert(variable.clone());
        }
        Term::Agg(_, inner) => collect_term_variables(inner, variables),
        _ => {}
    }
}

fn collect_expression_variables(expression: &Expr, variables: &mut BTreeSet<String>) {
    match expression {
        Expr::T(term) => collect_term_variables(term, variables),
        Expr::Add(left, right) | Expr::Sub(left, right) => {
            collect_expression_variables(left, variables);
            collect_expression_variables(right, variables);
        }
    }
}

fn reject_unbound(
    owner: &str,
    location: &str,
    used: &BTreeSet<String>,
    bound: &BTreeSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let unbound: Vec<&str> = used.difference(bound).map(String::as_str).collect();
    if unbound.is_empty() {
        Ok(())
    } else {
        Err(format!("unsafe rule {owner:?}: {location} variables {unbound:?} are unbound").into())
    }
}

fn term_can_resolve(term: &Term, bound: &BTreeSet<String>) -> bool {
    match term {
        Term::Var(variable) => bound.contains(variable),
        Term::Int(_) | Term::Sym(_) => true,
        Term::Wildcard | Term::Agg(..) => false,
    }
}

fn expression_can_resolve(expression: &Expr, bound: &BTreeSet<String>) -> bool {
    match expression {
        Expr::T(term) => term_can_resolve(term, bound),
        Expr::Add(left, right) | Expr::Sub(left, right) => {
            numeric_expression_can_resolve(left, bound)
                && numeric_expression_can_resolve(right, bound)
        }
    }
}

fn numeric_expression_can_resolve(expression: &Expr, bound: &BTreeSet<String>) -> bool {
    match expression {
        Expr::T(Term::Int(_)) => true,
        Expr::T(Term::Var(variable)) => bound.contains(variable),
        Expr::T(_) => false,
        Expr::Add(left, right) | Expr::Sub(left, right) => {
            numeric_expression_can_resolve(left, bound)
                && numeric_expression_can_resolve(right, bound)
        }
    }
}

fn linear_expression_binding(
    expression: &Expr,
    bound: &BTreeSet<String>,
) -> Option<(i64, Option<String>)> {
    match expression {
        Expr::T(Term::Int(_)) => Some((0, None)),
        Expr::T(Term::Var(variable)) if bound.contains(variable) => Some((0, None)),
        Expr::T(Term::Var(variable)) => Some((1, Some(variable.clone()))),
        Expr::T(_) => None,
        Expr::Add(left, right) => {
            let (left_coefficient, left_variable) = linear_expression_binding(left, bound)?;
            let (right_coefficient, right_variable) = linear_expression_binding(right, bound)?;
            combine_linear_bindings(
                left_coefficient.checked_add(right_coefficient)?,
                left_variable,
                right_variable,
            )
        }
        Expr::Sub(left, right) => {
            let (left_coefficient, left_variable) = linear_expression_binding(left, bound)?;
            let (right_coefficient, right_variable) = linear_expression_binding(right, bound)?;
            combine_linear_bindings(
                left_coefficient.checked_sub(right_coefficient)?,
                left_variable,
                right_variable,
            )
        }
    }
}

fn combine_linear_bindings(
    coefficient: i64,
    left: Option<String>,
    right: Option<String>,
) -> Option<(i64, Option<String>)> {
    let variable = match (left, right) {
        (None, None) => None,
        (Some(variable), None) | (None, Some(variable)) => Some(variable),
        (Some(_), Some(_)) => return None,
    };
    Some((coefficient, variable))
}
