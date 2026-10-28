//! The evaluation core: fact store, semiring annotations, stratification,
//! seminaive fixpoint evaluation, and provenance tracking.

mod store;
mod stratify;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::ast::{parse_program, Atom, Clause, CmpOp, Lit};
use crate::intern::{Interner, Term, Value};

pub use store::{Ann, Change, Key, Relation, Row, StoredFact, Support};
pub(crate) use store::{Emission, InsertOutcome};
pub(crate) use stratify::check_expression;
pub use stratify::StratError;

// ------------------------------------------------------------- environment

#[derive(Debug, Clone, Default)]
/// Backtracking environment with an undo trail (WAM-style): bindings are
/// recorded so a join can undo them on backtrack instead of cloning the
/// whole environment per candidate.
struct Env {
    map: HashMap<String, Value>,
    ann: Ann,
    body_keys: Vec<Key>,
    trail: Vec<(String, Option<Value>)>,
}

/// A backtrack point: trail length, body-key length, annotation snapshot.
#[derive(Clone)]
struct Mark {
    trail: usize,
    body: usize,
    ann: Ann,
}

impl Env {
    fn new() -> Self {
        Env {
            map: HashMap::new(),
            ann: Ann::unit(),
            body_keys: Vec::new(),
            trail: Vec::new(),
        }
    }

    fn bind(&mut self, v: &str, val: Value) {
        self.trail.push((v.to_string(), self.map.get(v).copied()));
        self.map.insert(v.to_string(), val);
    }

    fn mark(&self) -> Mark {
        Mark {
            trail: self.trail.len(),
            body: self.body_keys.len(),
            ann: self.ann.clone(),
        }
    }

    fn undo(&mut self, m: &Mark) {
        while self.trail.len() > m.trail {
            let (v, old) = self.trail.pop().unwrap();
            match old {
                Some(prev) => {
                    self.map.insert(v, prev);
                }
                None => {
                    self.map.remove(&v);
                }
            }
        }
        self.body_keys.truncate(m.body);
        self.ann = m.ann.clone();
    }

    fn lookup(&self, t: &Term) -> Option<Value> {
        match t {
            Term::Var(v) => self.map.get(v).copied(),
            _ => None,
        }
    }
}

pub struct Engine {
    pub interner: Interner,
    pub relations: HashMap<String, Relation>,
    pub clauses: Vec<Clause>,
    pub now: i64,
    /// Facts derived by the most recent ask_deep (demand-slice size).
    pub last_demand_facts: usize,
    /// Facts the most recent `hypothetical` would have added.
    pub last_hypothetical_facts: usize,
    /// Append-only log of newly created facts: (epoch, key). Backs
    /// `changes_from` for context assembly ("what changed in memory").
    pub change_log: Vec<(u64, Key)>,
    /// Full change feed (adds, retractions, clears) for streaming
    /// projections.
    pub feed: Vec<Change>,
    /// Rule registry: (batch id, source, clause range end).
    pub rule_batches: Vec<(String, String, usize)>,
    /// Monotonic batch id source: `rule_batches.len()` would collide
    /// after an uninstall + install in one session (two live batches
    /// sharing an id makes `uninstall` target the wrong one).
    pub batch_counter: usize,
    /// Set when the program changed since the last run; the next `run()`
    /// clears and rebuilds every derived relation (backfill).
    pub program_dirty: bool,
    /// Predicates that have EVER had a defining rule: an over-approximation
    /// used to clear orphaned derivations after `uninstall`.
    pub(crate) ever_derived: BTreeSet<String>,
    /// change_log length at the end of the last completed `run()`: the
    /// additions-since window used to detect growth of negated predicates.
    last_run_log_len: usize,
    epoch: u64,
    /// Predicates retracted since the last `run()`; triggers a scoped
    /// recompute of their transitive dependents.
    retracted: BTreeSet<String>,
    /// Whether to record streaming `feed` events (disabled in one-shot batch mode).
    record_feed: bool,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            interner: Interner::new(),
            relations: HashMap::new(),
            clauses: Vec::new(),
            now: 0,
            last_demand_facts: 0,
            last_hypothetical_facts: 0,
            change_log: Vec::new(),
            feed: Vec::new(),
            rule_batches: Vec::new(),
            batch_counter: 0,
            program_dirty: true,
            ever_derived: BTreeSet::new(),
            last_run_log_len: 0,
            epoch: 0,
            retracted: BTreeSet::new(),
            record_feed: true,
        }
    }

    /// Construct a batch-mode engine for one-shot artifact evaluation,
    /// skipping streaming change-feed allocations.
    pub(crate) fn new_batch() -> Self {
        let mut engine = Self::new();
        engine.record_feed = false;
        engine
    }

    /// Install a versioned batch of rules and facts from source text.
    pub fn install_program(&mut self, source: &str) -> Result<String, Box<dyn std::error::Error>> {
        let clauses = parse_program(source)?;
        self.install_clauses(clauses, source.to_string())
    }

    /// Install pre-parsed clauses directly without re-tokenizing or re-parsing.
    pub(crate) fn install_clauses(
        &mut self,
        clauses: Vec<Clause>,
        source: String,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.validate_aggregates(&clauses)?;
        self.validate_rule_safety(&clauses)?;
        for clause in &clauses {
            for literal in &clause.body {
                if let Lit::Cmp(_, left, right) = literal {
                    self.intern_comparison_constant(left);
                    if let crate::ast::Expr::T(term) = right {
                        self.intern_comparison_constant(term);
                    }
                }
            }
        }
        let previous_clause_count = self.clauses.len();
        let previous_ever_derived = self.ever_derived.clone();
        for (offset, clause) in clauses.iter().enumerate() {
            if !clause.is_fact {
                self.ever_derived.insert(clause.head.pred.clone());
                if clause
                    .head
                    .args
                    .iter()
                    .any(|term| matches!(term, Term::Agg(..)))
                {
                    let temp_clause = self.clauses.len() + offset;
                    self.ever_derived
                        .insert(format!("__agg:{}:{temp_clause}", clause.head.pred));
                }
            }
        }
        self.clauses.extend(clauses);
        if let Err(error) = self.check_program() {
            self.clauses.truncate(previous_clause_count);
            self.ever_derived = previous_ever_derived;
            return Err(error);
        }
        let id = format!("b{}", self.batch_counter);
        self.batch_counter += 1;
        self.rule_batches
            .push((id.clone(), source, self.clauses.len()));
        self.program_dirty = true;
        Ok(id)
    }

    // ------------------------------------------------------------- EDB API

    /// Assert a base fact with confidence and provenance.
    pub fn declare(&mut self, pred: &str, args: &[Value], ann: Ann) -> bool {
        let rel = self.relations.entry(pred.to_string()).or_default();
        let outcome = rel.insert(
            args.to_vec(),
            StoredFact {
                ann,
                supports: vec![Support::Base],
            },
        );
        match outcome {
            InsertOutcome::New => {
                let key = (pred.to_string(), args.to_vec());
                if self.record_feed {
                    self.feed.push(Change::Added(self.epoch, key.clone()));
                }
                self.change_log.push((self.epoch, key));
            }
            InsertOutcome::Improved => {
                if self.record_feed {
                    self.feed.push(Change::Updated(
                        self.epoch,
                        (pred.to_string(), args.to_vec()),
                    ));
                }
            }
            InsertOutcome::Unchanged => {}
        }
        outcome == InsertOutcome::New
    }

    /// Hypothetical evaluation: assert `extra` base facts, run to
    /// fixpoint, answer `goal`, then restore the store byte-identically —
    /// the "what follows if we assume X?" lookahead primitive (design
    /// §4.5). `last_hypothetical_facts` reports how many facts the
    /// assumption would have added.
    pub fn hypothetical(
        &mut self,
        extra: &[(&str, &[Value])],
        goal: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        // validate the goal before touching anything
        let prog = crate::ast::parse_program(&format!("{goal}."))?;
        if prog.len() != 1 || !prog[0].is_fact {
            return Err(crate::ast::ParseError(format!(
                "hypothetical: expected a single atom, got {goal:?}"
            ))
            .into());
        }
        let backup_relations = self.relations.clone();
        let backup_log = self.change_log.clone();
        let backup_feed = self.feed.clone();
        let backup_retracted = self.retracted.clone();
        let backup_dirty = self.program_dirty;
        let backup_log_len = self.last_run_log_len;
        let backup_epoch = self.epoch;
        let log_mark = self.change_log.len();

        for (pred, args) in extra {
            self.declare(pred, args, Ann::unit());
        }
        self.run();
        self.last_hypothetical_facts = self.change_log.len() - log_mark;
        let rows = self.ask_atom(&prog[0].head);

        self.relations = backup_relations;
        self.change_log = backup_log;
        self.feed = backup_feed;
        self.retracted = backup_retracted;
        self.program_dirty = backup_dirty;
        self.last_run_log_len = backup_log_len;
        self.epoch = backup_epoch;
        Ok(rows)
    }

    /// Facts created in the given epoch or later (inclusive): the window a
    /// turn's assertions and derivations are logged under, since the epoch
    /// counter only advances at the end of `run()`.
    pub fn changes_from(&self, epoch: u64) -> Vec<Key> {
        self.change_log
            .iter()
            .filter(|(e, _)| *e >= epoch)
            .map(|(_, k)| k.clone())
            .collect()
    }

    pub fn sym(&mut self, s: &str) -> Value {
        Value::Sym(self.interner.intern(s))
    }

    /// Comparisons resolve constants by lookup, so a symbol no fact mentions
    /// must be interned or `X \= c` could never hold.
    fn intern_comparison_constant(&mut self, term: &Term) {
        if let Term::Sym(symbol) = term {
            self.interner.intern(symbol);
        }
    }

    /// Retract a base (EDB) fact. Used for supersession when the old tuple
    /// must be replaced (e.g. re-asserting an edge with a closed `valid_to`).
    /// The next `run()` recomputes only the transitive dependents of the
    /// retracted predicate (scoped negative delta); unrelated derived
    /// relations keep their incrementality.
    pub fn retract(&mut self, pred: &str, args: &[Value]) -> bool {
        if let Some(rel) = self.relations.get_mut(pred) {
            let existed = rel.remove(args);
            rel.pending.remove(args);
            if existed {
                if self.record_feed {
                    self.feed.push(Change::Retracted(
                        self.epoch,
                        (pred.to_string(), args.to_vec()),
                    ));
                }
                self.retracted.insert(pred.to_string());
            }
            existed
        } else {
            false
        }
    }

    /// The streaming change feed: all events logged at or after the given
    /// checkpoint epoch. Events asserted between runs are stamped with the
    /// current epoch (the counter only advances at the end of `run()`), so
    /// checkpoint at `epoch()` right after a run and everything from the
    /// next turn's window arrives — additions, retractions, and wholesale
    /// clears — exactly what an external projection needs to stay in sync.
    pub fn changes_since(&self, epoch: u64) -> Vec<Change> {
        self.feed
            .iter()
            .filter(|c| c.epoch() >= epoch)
            .cloned()
            .collect()
    }

    pub fn set_now(&mut self, now: i64) {
        if self.now != now {
            self.now = now;
            self.program_dirty = true;
        }
    }

    // -------------------------------------------------------------- fixpoint

    /// Run (or incrementally re-run) the program. Returns the number of new
    /// facts derived this epoch. Pending deltas are consumed: a subsequent
    /// `run()` with no new assertions derives nothing.
    pub fn run(&mut self) -> usize {
        let strata = match self.strata() {
            Ok(s) => s,
            Err(_) => return 0, // use check_program to surface the error
        };
        if self.program_dirty {
            self.program_recompute(&strata);
        }
        // program-embedded facts
        let fact_clauses: Vec<Clause> =
            self.clauses.iter().filter(|c| c.is_fact).cloned().collect();
        for c in &fact_clauses {
            if let Some(args) = self.ground_args(&c.head.args) {
                self.declare(&c.head.pred, &args, Ann::unit());
            }
        }

        // main evaluation: strata in order (negated predicates are always
        // lower-stratum or EDB, so within-run negation reads are complete)
        let mut derived = 0;
        for stratum in &strata {
            derived += self.eval_stratum(stratum);
        }
        // invalidation passes AFTER evaluation: retractions (supersession)
        // and cross-run growth of negated predicates leave stale derived
        // rows; clear and rebuild the affected slices now that all inputs
        // are materialized. Loop to a fixpoint (aggregate value changes
        // mark their heads retracted and cascade here too).
        let neg_rules = self
            .clauses
            .iter()
            .any(|c| !c.is_fact && c.body.iter().any(|l| matches!(l, Lit::Neg(_))));
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > strata.len() + 2 {
                break;
            }
            let mut extra_clear: BTreeSet<String> = BTreeSet::new();
            if neg_rules {
                let neg_readers: BTreeSet<(String, String)> = self
                    .clauses
                    .iter()
                    .filter(|c| !c.is_fact)
                    .flat_map(|c| {
                        c.body.iter().filter_map(move |l| match l {
                            Lit::Neg(a) => Some((a.pred.clone(), c.head.pred.clone())),
                            _ => None,
                        })
                    })
                    .collect();
                let grew: BTreeSet<&String> = self.change_log[self.last_run_log_len..]
                    .iter()
                    .map(|(_, (p, _))| p)
                    .collect();
                for (neg_pred, reader) in &neg_readers {
                    if grew.contains(neg_pred) {
                        extra_clear.insert(reader.clone());
                    }
                }
            }
            if self.retracted.is_empty() && extra_clear.is_empty() {
                break;
            }
            let (_, d) = self.scoped_recompute(&strata, extra_clear);
            derived += d;
        }
        self.last_run_log_len = self.change_log.len();
        for rel in self.relations.values_mut() {
            rel.pending.clear();
        }
        self.epoch += 1;
        derived
    }

    /// Scoped negative delta, DRed-style: process dependent predicates
    /// level by level (stratum order). Clear and rebuild a dependent only
    /// if the key set of what it reads actually changed — a works_at
    /// supersession rebuilds `current` (linear) but leaves a closure that
    /// only reads the manager slice untouched. Propagation continues to
    /// deeper dependents only through predicates whose contents changed.
    fn scoped_recompute(
        &mut self,
        strata: &[Vec<usize>],
        extra_clear: BTreeSet<String>,
    ) -> (BTreeSet<usize>, usize) {
        // direct readers: pred -> IDB preds whose rules read it (pos/neg).
        // Aggregation clauses read via their temp relation and fold:
        // body -> temp -> head.
        let mut direct_readers: BTreeMap<String, BTreeSet<String>> = Default::default();
        for (ci, c) in self.clauses.iter().enumerate() {
            if c.is_fact {
                continue;
            }
            if Self::is_agg_clause(c) {
                let temp = self.agg_temp_pred(ci);
                for lit in &c.body {
                    if let Lit::Pos(a) | Lit::Neg(a) = lit {
                        direct_readers
                            .entry(a.pred.clone())
                            .or_default()
                            .insert(temp.clone());
                    }
                }
                direct_readers
                    .entry(temp)
                    .or_default()
                    .insert(c.head.pred.clone());
            } else {
                for lit in &c.body {
                    if let Lit::Pos(a) | Lit::Neg(a) = lit {
                        direct_readers
                            .entry(a.pred.clone())
                            .or_default()
                            .insert(c.head.pred.clone());
                    }
                }
            }
        }
        let mut idb: BTreeSet<String> = strata
            .iter()
            .flat_map(|s| s.iter().map(|&i| self.clauses[i].head.pred.clone()))
            .collect();
        // aggregation temp relations are derived state too (they are not
        // clause heads in strata, so add them explicitly)
        for ci in 0..self.clauses.len() {
            if !self.clauses[ci].is_fact && Self::is_agg_clause(&self.clauses[ci]) {
                idb.insert(self.agg_temp_pred(ci));
            }
        }
        // pending-clear worklist starts at direct IDB dependents of
        // retracted predicates, plus any explicitly requested clears
        let mut to_clear: BTreeSet<String> = self
            .retracted
            .iter()
            .chain(extra_clear.iter())
            .flat_map(|p| direct_readers.get(p).cloned().unwrap_or_default())
            .filter(|p| idb.contains(p))
            .collect();
        for p in extra_clear {
            if idb.contains(&p) {
                to_clear.insert(p);
            }
        }
        self.retracted.clear();
        let mut evaluated: BTreeSet<usize> = BTreeSet::new();
        let mut derived = 0usize;
        // fixpoint over rounds: a change can queue readers in an
        // already-processed stratum (same-depth positive dependency), which
        // demands another pass
        let mut round = 0usize;
        let mut preexisting: BTreeMap<String, BTreeSet<Vec<Value>>> = Default::default();
        while !to_clear.is_empty() && round < strata.len() + 2 {
            round += 1;
            for (si, stratum) in strata.iter().enumerate() {
                if to_clear.is_empty() {
                    break;
                }
                let mut here: BTreeSet<String> = stratum
                    .iter()
                    .map(|&i| self.clauses[i].head.pred.clone())
                    .filter(|p| to_clear.contains(p))
                    .collect();
                // aggregation temps attach to their clause's stratum
                for &i in stratum {
                    if !self.clauses[i].is_fact && Self::is_agg_clause(&self.clauses[i]) {
                        let t = self.agg_temp_pred(i);
                        if to_clear.contains(&t) {
                            here.insert(t);
                        }
                    }
                }
                if here.is_empty() {
                    continue;
                }
                to_clear = to_clear.difference(&here).cloned().collect();
                // snapshots to detect actual change
                let mut snapshots: BTreeMap<String, BTreeSet<Vec<Value>>> = Default::default();
                for p in &here {
                    if let Some(rel) = self.relations.get(p) {
                        snapshots
                            .insert(p.clone(), rel.rows.iter().map(|r| r.key.clone()).collect());
                    }
                }
                for p in &here {
                    if let Some(rel) = self.relations.get_mut(p) {
                        preexisting
                            .entry(p.clone())
                            .or_insert_with(|| rel.rows.iter().map(|r| r.key.clone()).collect());
                        if !rel.is_empty() && self.record_feed {
                            self.feed.push(Change::Cleared(self.epoch, p.clone()));
                        }
                        rel.clear();
                    }
                }
                // seed: full contents of the body predicates of these rules
                // that were not just cleared
                let mut seeds: BTreeSet<String> = BTreeSet::new();
                for (ci, c) in self.clauses.iter().enumerate() {
                    if c.is_fact {
                        continue;
                    }
                    let owner = if Self::is_agg_clause(c) {
                        self.agg_temp_pred(ci)
                    } else {
                        c.head.pred.clone()
                    };
                    if !here.contains(&owner) {
                        continue;
                    }
                    for lit in &c.body {
                        if let Lit::Pos(a) | Lit::Neg(a) = lit {
                            if !here.contains(&a.pred) {
                                seeds.insert(a.pred.clone());
                            }
                        }
                    }
                }
                for p in seeds {
                    if let Some(rel) = self.relations.get_mut(&p) {
                        let keys: Vec<Vec<Value>> =
                            rel.rows.iter().map(|r| r.key.clone()).collect();
                        rel.pending.extend(keys);
                    }
                }
                derived += self.eval_stratum(stratum);
                evaluated.insert(si);
                // changed predicates propagate clearing to their readers
                for p in &here {
                    let now_keys: BTreeSet<Vec<Value>> = self
                        .relations
                        .get(p)
                        .map(|r| r.rows.iter().map(|x| x.key.clone()).collect())
                        .unwrap_or_default();
                    let changed = now_keys != snapshots.get(p).cloned().unwrap_or_default();
                    if changed {
                        if let Some(readers) = direct_readers.get(p) {
                            for r in readers {
                                // eval_stratum already fixpoints within a
                                // stratum (self-recursion and same-SCC readers
                                // see the final state); requeueing them only
                                // buys a redundant full-rebuild round
                                if r != p && !here.contains(r) && idb.contains(r) {
                                    to_clear.insert(r.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        // multi-round re-derivations of facts that existed before the
        // recompute are not new facts
        for (p, keys) in &preexisting {
            if let Some(rel) = self.relations.get(p) {
                let still: usize = keys.iter().filter(|k| rel.contains(k)).count();
                derived = derived.saturating_sub(still);
            }
        }
        (evaluated, derived)
    }

    /// Does this clause aggregate (has Agg terms in its head)?
    pub(crate) fn is_agg_clause(c: &Clause) -> bool {
        c.head.args.iter().any(|t| matches!(t, Term::Agg(..)))
    }

    /// Stable temp predicate for an aggregation clause's lowered body
    /// solutions: `__agg:{head_pred}:{clause_index}`.
    fn agg_temp_pred(&self, ci: usize) -> String {
        format!("__agg:{}:{ci}", self.clauses[ci].head.pred)
    }

    /// Lower an aggregation clause: head = temp(group args..., inner
    /// terms...); body unchanged. The ordinary evaluator enumerates all
    /// body solutions into the temp relation (set semantics = distinct
    /// (group, value) pairs, which is exactly COUNT(DISTINCT) and is
    /// idempotent for min/max/sum).
    fn lower_agg_clause(&self, ci: usize) -> Clause {
        let c = &self.clauses[ci];
        let mut args = Vec::new();
        for t in &c.head.args {
            match t {
                Term::Agg(_, inner) => args.push((**inner).clone()),
                other => args.push(other.clone()),
            }
        }
        Clause {
            name: c.name.clone(),
            head: Atom {
                pred: self.agg_temp_pred(ci),
                args,
            },
            body: c.body.clone(),
            is_fact: false,
        }
    }

    /// Fold one aggregation clause's temp relation into its head
    /// predicate. Groups are the temp relation's non-agg prefix; each agg
    /// position folds over its suffix column. Changed values replace the
    /// old rows (key change) so downstream recomputes propagate.
    fn fold_agg_clause(&mut self, ci: usize) -> usize {
        let c = self.clauses[ci].clone();
        let temp = self.agg_temp_pred(ci);
        let Some(rel) = self.relations.get(&temp) else {
            return 0;
        };
        // collect group data under this borrow; the fold writes below
        // need the borrow released
        let groups = {
            let group_len = c
                .head
                .args
                .iter()
                .filter(|t| !matches!(t, Term::Agg(..)))
                .count();
            let mut g: BTreeMap<Vec<Value>, Vec<Vec<Value>>> = Default::default();
            for row in &rel.rows {
                let key = row.key[..group_len].to_vec();
                g.entry(key)
                    .or_default()
                    .push(row.key[group_len..].to_vec());
            }
            g
        };
        let fns: Vec<crate::intern::AggFn> = c
            .head
            .args
            .iter()
            .filter_map(|t| match t {
                Term::Agg(f, _) => Some(*f),
                _ => None,
            })
            .collect();
        let mut changed = 0usize;
        for (g, rows) in groups {
            let mut out = g.clone();
            for (ai, f) in fns.iter().enumerate() {
                let vals: Vec<i64> = rows
                    .iter()
                    .filter_map(|r| r.get(ai).and_then(|v| v.as_int()))
                    .collect();
                let folded = match f {
                    crate::intern::AggFn::Count => rows.len() as i64,
                    crate::intern::AggFn::Min => vals.iter().min().copied().unwrap_or(0),
                    crate::intern::AggFn::Max => vals.iter().max().copied().unwrap_or(0),
                    crate::intern::AggFn::Sum => vals.iter().sum(),
                };
                out.push(Value::Int(folded));
            }
            // replace any existing row with the same group prefix
            let mut pattern: Vec<Option<Value>> = g.iter().map(|v| Some(*v)).collect();
            for _ in 0..fns.len() {
                pattern.push(None);
            }
            let old: Vec<Vec<Value>> = self
                .relations
                .get(&c.head.pred)
                .map(|r| {
                    r.lookup(&pattern)
                        .into_iter()
                        .map(|id| r.rows[id].key.clone())
                        .collect()
                })
                .unwrap_or_default();
            for o in old {
                if o != out {
                    self.retract(&c.head.pred, &o);
                    changed += 1;
                }
            }
            let exists = self
                .relations
                .get(&c.head.pred)
                .map(|r| r.contains(&out))
                .unwrap_or(false);
            if !exists {
                // witness: the first contributing temp row
                let support = Support::Rule {
                    rule: c
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("agg:{}", c.head.pred)),
                    body: rows
                        .first()
                        .map(|r| {
                            let mut k = g.clone();
                            k.extend(r.iter().copied());
                            (temp.clone(), k)
                        })
                        .into_iter()
                        .collect(),
                };
                let outcome = self
                    .relations
                    .entry(c.head.pred.clone())
                    .or_default()
                    .insert(
                        out.clone(),
                        StoredFact {
                            ann: Ann::unit(),
                            supports: vec![support],
                        },
                    );
                if outcome == InsertOutcome::New {
                    if self.record_feed {
                        self.feed.push(Change::Added(
                            self.epoch,
                            (c.head.pred.clone(), out.clone()),
                        ));
                    }
                    self.change_log
                        .push((self.epoch, (c.head.pred.clone(), out)));
                    changed += 1;
                }
            }
        }
        changed
    }

    /// Read-only iteration over (predicate, relation).
    pub fn relations_iter(&self) -> impl Iterator<Item = (&String, &Relation)> {
        self.relations.iter()
    }

    /// All keys of one relation (read-only convenience).
    pub fn relation_keys(&self, pred: &str) -> Vec<Vec<Value>> {
        self.relations
            .get(pred)
            .map(|r| r.rows.iter().map(|x| x.key.clone()).collect())
            .unwrap_or_default()
    }

    /// Schema summary for rule authoring: each predicate with arity, row
    /// count, and up to three sample facts.
    pub fn schema_summary(&self) -> String {
        let mut out = String::new();
        let mut preds: Vec<&String> = self.relations.keys().collect();
        preds.sort();
        for p in preds {
            let rel = &self.relations[p];
            let arity = rel.rows.first().map(|r| r.key.len()).unwrap_or(0);
            let _ = writeln!(out, "{}/{}: {} rows", p, arity, rel.len());
            for row in rel.rows.iter().take(3) {
                let _ = writeln!(out, "  {}", self.render_fact(p, &row.key));
            }
        }
        out
    }

    /// Installed rule batches: (id, source).
    pub fn batches(&self) -> Vec<(String, String)> {
        self.rule_batches
            .iter()
            .map(|(id, src, _)| (id.clone(), src.clone()))
            .collect()
    }

    /// Uninstall a rule batch (revertable rules). Derived facts are
    /// recomputed on the next `run()` without the removed rules.
    pub fn uninstall(&mut self, id: &str) -> bool {
        let Some(pos) = self.rule_batches.iter().position(|(b, _, _)| b == id) else {
            return false;
        };
        let (_, _, end) = self.rule_batches.remove(pos);
        let start = match pos.checked_sub(1).and_then(|i| self.rule_batches.get(i)) {
            Some((_, _, prev_end)) => *prev_end,
            None => 0,
        };
        if start <= end && end <= self.clauses.len() {
            self.clauses.drain(start..end);
            // renumber the ends of later batches
            let removed = end - start;
            for (_, _, e) in self.rule_batches.iter_mut() {
                if *e > end {
                    *e -= removed;
                }
            }
        }
        self.program_dirty = true;
        true
    }

    /// Program-change recompute: clear every derived relation and reseed
    /// from all base facts, so newly installed rules backfill against the
    /// existing store and uninstalled rules' derivations disappear.
    fn program_recompute(&mut self, strata: &[Vec<usize>]) {
        let mut to_clear: BTreeSet<String> = self.ever_derived.clone();
        for s in strata {
            for &i in s {
                to_clear.insert(self.clauses[i].head.pred.clone());
            }
        }
        for p in &to_clear {
            if let Some(rel) = self.relations.get_mut(p) {
                if !rel.is_empty() && self.record_feed {
                    self.feed.push(Change::Cleared(self.epoch, p.clone()));
                }
                rel.clear();
            }
        }
        for rel in self.relations.values_mut() {
            let keys: Vec<Vec<Value>> = rel.rows.iter().map(|r| r.key.clone()).collect();
            rel.pending.extend(keys);
        }
        self.program_dirty = false;
    }

    /// Validate the installed program (parse + stratification).
    pub fn check_program(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.strata()?;
        Ok(())
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Seminaive evaluation of one stratum.
    fn eval_stratum(&mut self, clause_idx: &[usize]) -> usize {
        // aggregation clauses are lowered: the ordinary loop populates
        // their temp relation; the fold step afterwards materializes the
        // aggregated head facts
        let clauses: Vec<Clause> = clause_idx
            .iter()
            .map(|&i| {
                if Self::is_agg_clause(&self.clauses[i]) {
                    self.lower_agg_clause(i)
                } else {
                    self.clauses[i].clone()
                }
            })
            .collect();
        let agg_clauses: Vec<usize> = clause_idx
            .iter()
            .copied()
            .filter(|&i| Self::is_agg_clause(&self.clauses[i]))
            .collect();
        let mut delta: HashMap<String, BTreeSet<Vec<Value>>> = HashMap::new();
        for (pred, rel) in &self.relations {
            if !rel.pending.is_empty() {
                delta.insert(pred.clone(), rel.pending.clone());
            }
        }
        let mut total_new = 0usize;
        loop {
            let mut emissions: Vec<Emission> = Vec::new();
            for clause in &clauses {
                let pos: Vec<usize> = clause
                    .body
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| matches!(l, Lit::Pos(_)))
                    .map(|(i, _)| i)
                    .collect();
                if pos.is_empty() {
                    // no relational atoms: fire at most once per binding of
                    // builtins (which are ground-valued, so once)
                    if let Some(env) = self.solve_builtin_only(clause) {
                        if let Some(k) = self.emit_head(clause, &env) {
                            emissions.push(k);
                        }
                    }
                    continue;
                }
                for &ai in &pos {
                    self.fire_delta_version(clause, ai, &delta, &mut emissions);
                }
            }
            if emissions.is_empty() {
                break;
            }
            total_new += emissions.iter().filter(|emission| emission.is_new).count();
            delta.clear();
            for Emission { key: (p, a), .. } in emissions {
                delta.entry(p).or_default().insert(a);
            }
        }
        for ci in agg_clauses {
            self.fold_agg_clause(ci);
        }

        total_new
    }

    /// Fire one delta version of a rule: body atom `ai` binds against delta
    /// tuples; all other relational atoms scan full relations.
    fn fire_delta_version(
        &mut self,
        clause: &Clause,
        ai: usize,
        delta: &HashMap<String, BTreeSet<Vec<Value>>>,
        out: &mut Vec<Emission>,
    ) {
        let atom = match &clause.body[ai] {
            Lit::Pos(a) => a,
            _ => return,
        };
        let delta_keys: Vec<Vec<Value>> = match delta.get(&atom.pred) {
            Some(s) => s.iter().cloned().collect(),
            None => return,
        };
        let mut env = Env::new();
        let outer = env.mark();
        for k in delta_keys {
            if !self.bind_args(&atom.args, &k, &mut env) {
                env.undo(&outer);
                continue;
            }
            let ann = match self.relations.get(&atom.pred).and_then(|r| r.get(&k)) {
                Some(f) => f.ann.clone(),
                None => {
                    env.undo(&outer);
                    continue;
                }
            };
            env.ann = Ann::unit().join(&ann);
            env.body_keys.push((atom.pred.clone(), k));
            self.solve_rest(clause, 0, Some(ai), &mut env, out);
            env.undo(&outer);
        }
    }

    fn solve_builtin_only(&mut self, clause: &Clause) -> Option<Env> {
        let mut env = Env::new();
        let mut out = Vec::new();
        let impossible_skip = usize::MAX;
        if self.solve_all(clause, 0, impossible_skip, &mut env, &mut out) {
            let _ = out;
            Some(env)
        } else {
            None
        }
    }

    /// Resolve body lits from index `i` onward. `skip` bypasses an atom
    /// (already bound by the delta version). Emits heads; returns whether at
    /// least one solution was found.
    fn solve_rest(
        &mut self,
        clause: &Clause,
        i: usize,
        skip: Option<usize>,
        env: &mut Env,
        out: &mut Vec<Emission>,
    ) -> bool {
        self.solve_all(clause, i, skip.unwrap_or(usize::MAX), env, out)
    }

    fn solve_all(
        &mut self,
        clause: &Clause,
        i: usize,
        skip: usize,
        env: &mut Env,
        out: &mut Vec<Emission>,
    ) -> bool {
        if i == clause.body.len() {
            if let Some(k) = self.emit_head(clause, env) {
                out.push(k);
                return true;
            }
            return false;
        }
        if i == skip {
            return self.solve_all(clause, i + 1, skip, env, out);
        }
        match &clause.body[i] {
            Lit::Pos(atom) => {
                let bound = self.resolve_bound(&atom.args, env);
                let ids = match self.relations.get(&atom.pred) {
                    Some(rel) => rel.lookup(&bound),
                    None => Vec::new(),
                };
                let mut any = false;
                for id in ids {
                    let mark = env.mark();
                    let matched = {
                        let Some(rel) = self.relations.get(&atom.pred) else {
                            continue;
                        };
                        let row = &rel.rows[id];
                        if self.bind_args(&atom.args, &row.key, env) {
                            Some((row.key.clone(), row.fact.ann.clone()))
                        } else {
                            None
                        }
                    };
                    if let Some((k, ann)) = matched {
                        env.ann = env.ann.join(&ann);
                        env.body_keys.push((atom.pred.clone(), k));
                        if self.solve_all(clause, i + 1, skip, env, out) {
                            any = true;
                        }
                    }
                    env.undo(&mark);
                }
                any
            }
            Lit::Neg(atom) => {
                // negation-as-absence against the full relation; the stratum
                // is already complete for lower-stratum predicates
                if self.matches_any(atom, env) {
                    return false;
                }
                self.solve_all(clause, i + 1, skip, env, out)
            }
            Lit::Now(t) => match t {
                Term::Wildcard => self.solve_all(clause, i + 1, skip, env, out),
                Term::Int(v) => {
                    if *v == self.now {
                        self.solve_all(clause, i + 1, skip, env, out)
                    } else {
                        false
                    }
                }
                Term::Agg(..) => false, // aggregates never appear in bodies
                Term::Var(v) => match env.map.get(v) {
                    Some(Value::Int(x)) if *x == self.now => {
                        self.solve_all(clause, i + 1, skip, env, out)
                    }
                    Some(_) => false,
                    None => {
                        env.bind(v, Value::Int(self.now));
                        self.solve_all(clause, i + 1, skip, env, out)
                    }
                },
                Term::Sym(_) => false,
            },
            Lit::Cmp(op, a, b) => {
                use crate::ast::Expr;
                let av = self.resolve_term(a, env);
                // plain-term RHS (covers symbol equality) first
                if let Expr::T(bt) = b {
                    match (av, self.resolve_term(bt, env)) {
                        (Some(av), Some(bv)) => {
                            return if cmp_holds(*op, av, bv) {
                                self.solve_all(clause, i + 1, skip, env, out)
                            } else {
                                false
                            };
                        }
                        (None, Some(bv)) => {
                            if let Term::Var(v) = a {
                                env.bind(v, bv);
                                return self.solve_all(clause, i + 1, skip, env, out);
                            }
                            return false;
                        }
                        _ => {}
                    }
                }
                // arithmetic path: expr = coeff * X + const
                if let Some((coeff, c, var)) = linearize(b, env) {
                    match (av, coeff) {
                        (Some(av), 0) => {
                            return if cmp_holds(*op, av, Value::Int(c)) {
                                self.solve_all(clause, i + 1, skip, env, out)
                            } else {
                                false
                            };
                        }
                        (Some(av), 1) => {
                            // RHS is X + c; solvable for equality: av = X + c
                            if *op == CmpOp::Eq {
                                if let (Some(ai), Some(v)) = (av.as_int(), var) {
                                    if let Some(diff) = ai.checked_sub(c) {
                                        env.bind(&v, Value::Int(diff));
                                        return self.solve_all(clause, i + 1, skip, env, out);
                                    }
                                }
                            }
                        }
                        (None, 0) => {
                            if let Term::Var(v) = a {
                                env.bind(v, Value::Int(c));
                                return self.solve_all(clause, i + 1, skip, env, out);
                            }
                        }
                        _ => {}
                    }
                }
                false
            }
        }
    }

    /// Resolve a pattern's terms against env into concrete bound values
    /// (None = unbound). Unknown symbols resolve to a sentinel that matches
    /// nothing, so lookups short-circuit.
    fn resolve_bound(&self, pat: &[Term], env: &Env) -> Vec<Option<Value>> {
        pat.iter()
            .map(|t| match t {
                Term::Int(i) => Some(Value::Int(*i)),
                Term::Agg(..) => None, // never matches: aggregates are heads-only
                Term::Var(v) => env.map.get(v).copied(),
                Term::Sym(s) => match self.interner.lookup(s) {
                    Some(sym) => Some(Value::Sym(sym)),
                    None => Some(Value::Int(i64::MIN)), // unknown: matches nothing
                },
                Term::Wildcard => None,
            })
            .collect()
    }

    fn matches_any(&self, atom: &Atom, env: &mut Env) -> bool {
        let bound = self.resolve_bound(&atom.args, env);
        let Some(rel) = self.relations.get(&atom.pred) else {
            return false;
        };
        let ids = rel.lookup(&bound);
        let mark = env.mark();
        for id in ids {
            let k = &rel.rows[id].key;
            let matched = self.bind_args(&atom.args, k, env);
            env.undo(&mark);
            if matched {
                return true;
            }
        }
        false
    }

    fn resolve_term(&self, t: &Term, env: &Env) -> Option<Value> {
        match t {
            Term::Var(_) => env.lookup(t),
            Term::Int(i) => Some(Value::Int(*i)),
            Term::Sym(s) => self.interner.lookup(s).map(Value::Sym),
            Term::Wildcard => None,
            Term::Agg(..) => None, // heads-only; never resolved in bodies
        }
    }

    fn bind_args(&self, pat: &[Term], args: &[Value], env: &mut Env) -> bool {
        if pat.len() != args.len() {
            return false;
        }
        for (p, a) in pat.iter().zip(args) {
            let ok = match p {
                Term::Wildcard => true,
                Term::Agg(..) => false,
                Term::Var(v) => match env.map.get(v) {
                    Some(bound) => *bound == *a,
                    None => {
                        env.bind(v, *a);
                        true
                    }
                },
                Term::Int(i) => Value::Int(*i) == *a,
                Term::Sym(s) => matches!((self.interner.lookup(s), a),
                    (Some(sv), Value::Sym(av)) if sv == *av),
            };
            if !ok {
                return false;
            }
        }
        true
    }

    fn ground_args(&mut self, pat: &[Term]) -> Option<Vec<Value>> {
        pat.iter()
            .map(|p| match p {
                Term::Sym(s) => Some(Value::Sym(self.interner.intern(s))),
                Term::Int(i) => Some(Value::Int(*i)),
                _ => None,
            })
            .collect()
    }

    fn emit_head(&mut self, clause: &Clause, env: &Env) -> Option<Emission> {
        let mut args = Vec::with_capacity(clause.head.args.len());
        for t in &clause.head.args {
            let v = match t {
                Term::Sym(s) => Value::Sym(self.interner.intern(s)),
                Term::Int(i) => Value::Int(*i),
                Term::Var(v) => *env.map.get(v)?,
                Term::Wildcard => return None, // safety: no unbound heads
                Term::Agg(..) => return None,  // agg rules are lowered, never emitted
            };
            args.push(v);
        }
        let support = Support::Rule {
            rule: clause
                .name
                .clone()
                .unwrap_or_else(|| format!("rule/{}", clause.head.pred)),
            body: env.body_keys.clone(),
        };
        let rel = self.relations.entry(clause.head.pred.clone()).or_default();
        let outcome = rel.insert(
            args.clone(),
            StoredFact {
                ann: env.ann.clone(),
                supports: vec![support],
            },
        );
        match outcome {
            InsertOutcome::New => {
                let key = (clause.head.pred.clone(), args);
                if self.record_feed {
                    self.feed.push(Change::Added(self.epoch, key.clone()));
                }
                self.change_log.push((self.epoch, key.clone()));
                Some(Emission { key, is_new: true })
            }
            InsertOutcome::Improved => {
                let key = (clause.head.pred.clone(), args);
                if self.record_feed {
                    self.feed.push(Change::Updated(self.epoch, key.clone()));
                }
                Some(Emission { key, is_new: false })
            }
            InsertOutcome::Unchanged => None,
        }
    }

    // -------------------------------------------------------------- queries

    /// Query a predicate with a pattern; `None` slots are wildcards.
    /// Index-aware: bound positions select the smallest bucket instead of
    /// scanning the relation.
    pub fn query(&self, pred: &str, pattern: &[Option<Value>]) -> Vec<(Vec<Value>, Ann)> {
        let mut out = Vec::new();
        let Some(rel) = self.relations.get(pred) else {
            return out;
        };
        let ids: Vec<usize> = if pattern.iter().any(|p| p.is_some()) {
            rel.lookup(pattern)
        } else {
            (0..rel.rows.len()).collect()
        };
        for id in ids {
            let row = &rel.rows[id];
            if !pattern.is_empty() && pattern.len() != row.key.len() {
                return Vec::new();
            }
            if pattern
                .iter()
                .zip(&row.key)
                .all(|(p, v)| p.as_ref().map(|pv| pv == v).unwrap_or(true))
            {
                out.push((row.key.clone(), row.fact.ann.clone()));
            }
        }
        out
    }

    pub fn fact(&self, pred: &str, args: &[Value]) -> Option<StoredFact> {
        self.relations.get(pred).and_then(|r| r.get(args)).cloned()
    }

    /// Demand-driven query (magic sets): answers a goal like
    /// `reports_to("n0", Y)` WITHOUT materializing the full fixpoint of
    /// `reports_to`. The rewritten demand program is installed, evaluated,
    /// and removed; the base store is untouched. `last_demand_facts`
    /// records how many facts the demand evaluation derived (the
    /// demand-relevant slice) for observability.
    pub fn ask_deep(&mut self, goal: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let prog = crate::ast::parse_program(&format!("{goal}."))?;
        if prog.len() != 1 || !prog[0].is_fact {
            return Err(crate::ast::ParseError(format!(
                "ask_deep: expected a single atom, got {goal:?}"
            ))
            .into());
        }
        let head = &prog[0].head;
        // goal over an EDB predicate: no rewriting needed
        if !self.clauses.iter().any(|c| c.head.pred == head.pred) {
            return Ok(self.ask_atom(head));
        }
        // goal over a materialized derived predicate: read the store
        // directly (the demand machinery is for unmaterialized predicates;
        // callers keep materialization fresh via run/maintain)
        if self
            .relations
            .get(&head.pred)
            .map(|r| !r.is_empty())
            .unwrap_or(false)
        {
            return Ok(self.ask_atom(head));
        }
        let materialized: BTreeSet<String> = self
            .relations
            .iter()
            .filter(|(_, rel)| !rel.is_empty())
            .map(|(p, _)| p.clone())
            .collect();
        let demand = crate::magic::build(&self.clauses, head, &materialized)?;
        let relations_before: BTreeSet<String> = self.relations.keys().cloned().collect();
        let saved_clauses = self.clauses.clone();
        let saved_relations = self.relations.clone();
        let saved_change_log = self.change_log.clone();
        let saved_feed = self.feed.clone();
        let saved_retracted = self.retracted.clone();
        let saved_program_dirty = self.program_dirty;
        let saved_last_run_log_len = self.last_run_log_len;
        let saved_epoch = self.epoch;

        self.clauses = demand.clauses;
        let evaluation = (|| -> Result<(Vec<String>, usize), Box<dyn std::error::Error>> {
            self.check_program()?;
            self.run();

            let created = self
                .relations
                .iter()
                .filter(|(name, _)| !relations_before.contains(*name))
                .map(|(_, relation)| relation.len())
                .sum();
            let answer_atom = Atom {
                pred: demand.answer_pred,
                args: head.args.clone(),
            };
            let rows = self.ask_atom(&answer_atom);
            Ok((rows, created))
        })();

        self.clauses = saved_clauses;
        self.relations = saved_relations;
        self.change_log = saved_change_log;
        self.feed = saved_feed;
        self.retracted = saved_retracted;
        self.program_dirty = saved_program_dirty;
        self.last_run_log_len = saved_last_run_log_len;
        self.epoch = saved_epoch;

        let (rows, created) = evaluation?;
        self.last_demand_facts = created;
        Ok(rows)
    }

    /// Ask a conjunctive-free ground-or-varied atom, e.g.
    /// `current("alice", R, O)` or `reports_to(X, "carol")`, and get the
    /// variable bindings. Read-only, side-effect-free, terminating: the
    /// agent-facing query surface. Returns rendered rows like
    /// `R=works_at, O=acme`; a ground goal yields one empty-string row if
    /// the fact holds.
    pub fn ask(&self, goal: &str) -> Result<Vec<String>, crate::ast::ParseError> {
        let prog = crate::ast::parse_program(&format!("{goal}."))?;
        if prog.len() != 1 || !prog[0].is_fact {
            return Err(crate::ast::ParseError(format!(
                "ask: expected a single atom, got {goal:?}"
            )));
        }
        Ok(self.ask_atom(&prog[0].head))
    }

    /// Evaluate a pre-parsed query atom against the materialized store without re-parsing.
    pub fn ask_atom(&self, head: &Atom) -> Vec<String> {
        let vars: Vec<&String> = head
            .args
            .iter()
            .filter_map(|t| match t {
                Term::Var(v) => Some(v),
                _ => None,
            })
            .collect();
        let ground = head
            .args
            .iter()
            .all(|term| matches!(term, Term::Sym(_) | Term::Int(_)));
        let mut rows = Vec::new();
        if let Some(rel) = self.relations.get(&head.pred) {
            let env_empty = Env::new();
            let bound = self.resolve_bound(&head.args, &env_empty);
            let ids = if bound.iter().any(|b| b.is_some()) {
                rel.lookup(&bound)
            } else {
                (0..rel.rows.len()).collect()
            };
            let mut env = Env::new();
            let mark = env.mark();
            for id in ids {
                let k = &rel.rows[id].key;
                if self.bind_args(&head.args, k, &mut env) {
                    if ground {
                        return vec![String::new()];
                    }
                    let row: Vec<String> = vars
                        .iter()
                        .map(|v| {
                            let val = env
                                .map
                                .get(*v)
                                .map(|x| self.interner.display(x))
                                .unwrap_or_else(|| "_".into());
                            format!("{}={}", v, val)
                        })
                        .collect();
                    rows.push(row.join(", "));
                }
                env.undo(&mark);
            }
        }
        rows
    }

    // -------------------------------------------------------------- why

    /// Render a proof tree for a fact (cycle-safe).
    pub fn why(&self, pred: &str, args: &[Value]) -> String {
        let mut out = String::new();
        self.why_rec(pred, args, 0, &mut out, &mut BTreeSet::new());
        out
    }

    fn why_rec(
        &self,
        pred: &str,
        args: &[Value],
        indent: usize,
        out: &mut String,
        seen: &mut BTreeSet<Key>,
    ) {
        let pad = "  ".repeat(indent);
        let rendered = self.render_fact(pred, args);
        let f = match self.relations.get(pred).and_then(|r| r.get(args)) {
            Some(f) => f,
            None => {
                let _ = writeln!(out, "{pad}{rendered}  [unknown fact]");
                return;
            }
        };
        if !seen.insert((pred.to_string(), args.to_vec())) {
            let _ = writeln!(out, "{pad}{rendered}  [cycle: shown above]");
            return;
        }
        let prov: Vec<&String> = f.ann.prov.iter().collect();
        let _ = writeln!(
            out,
            "{pad}{rendered}  (conf {:.3}, prov {prov:?})",
            f.ann.conf
        );
        // render one witness derivation per distinct rule (further
        // witnesses of the same rule add no explanatory power)
        let mut shown_rules: BTreeSet<&str> = BTreeSet::new();
        let mut shown_any = false;
        for s in &f.supports {
            match s {
                Support::Base => {
                    if shown_rules.insert("«base»") {
                        let _ = writeln!(out, "{pad}  \u{21b3} asserted (base fact)");
                        shown_any = true;
                    }
                }
                Support::Rule { rule, body } => {
                    if shown_rules.insert(rule.as_str()) {
                        let _ = writeln!(out, "{pad}  \u{21b3} via {rule}");
                        for (p, a) in body {
                            self.why_rec(p, a, indent + 2, out, seen);
                        }
                        shown_any = true;
                    }
                }
            }
        }
        if !shown_any && !f.supports.is_empty() {
            let _ = writeln!(out, "{pad}  \u{21b3} (witnesses already shown above)");
        }
    }

    pub fn render_fact(&self, pred: &str, args: &[Value]) -> String {
        let rendered: Vec<String> = args.iter().map(|v| self.interner.display(v)).collect();
        format!("{}({})", pred, rendered.join(", "))
    }
}

/// Linearize an additive integer expression around its unique unbound
/// variable: returns (coeff, const, Some(var)) where expr = coeff*var+const,
/// or (0, value, None) when fully bound. None when not linear / two unbound
/// variables / symbol-typed.
fn linearize(e: &crate::ast::Expr, env: &Env) -> Option<(i64, i64, Option<String>)> {
    use crate::ast::Expr;
    match e {
        Expr::T(Term::Int(i)) => Some((0, *i, None)),
        Expr::T(Term::Var(v)) => match env.map.get(v) {
            Some(Value::Int(x)) => Some((0, *x, None)),
            None => Some((1, 0, Some(v.clone()))),
            Some(Value::Sym(_)) => None,
        },
        Expr::T(_) => None,
        Expr::Add(a, b) => {
            let (c1, k1, v1) = linearize(a, env)?;
            let (c2, k2, v2) = linearize(b, env)?;
            combine(v1, v2, c1.checked_add(c2)?, k1.checked_add(k2)?)
        }
        Expr::Sub(a, b) => {
            let (c1, k1, v1) = linearize(a, env)?;
            let (c2, k2, v2) = linearize(b, env)?;
            combine(v1, v2, c1.checked_sub(c2)?, k1.checked_sub(k2)?)
        }
    }
}

fn combine(
    v1: Option<String>,
    v2: Option<String>,
    c: i64,
    k: i64,
) -> Option<(i64, i64, Option<String>)> {
    let v = match (v1, v2) {
        (None, None) => None,
        (Some(v), None) | (None, Some(v)) => Some(v),
        (Some(_), Some(_)) => return None, // two unbound vars: not linear
    };
    Some((c, k, v))
}

fn cmp_holds(op: CmpOp, a: Value, b: Value) -> bool {
    match op {
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
    }
}
