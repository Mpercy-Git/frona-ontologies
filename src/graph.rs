//! The intermediate graph a source is parsed into, and the operations the recipes
//! compose over it.
//!
//! IRIs are interned to `u32` and every per-term collection is a dense `Vec` indexed
//! by that id. This is what makes the ancestor walk an integer chase and stops the same
//! IRI being stored once per map it appears in — interning cut peak memory 46% when this
//! was prototyped against KBpedia.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::path::Path;

use anyhow::{Context, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxrdf::{NamedOrBlankNode, Term};

use crate::rdf::*;

pub type Id = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Class,
    ObjectProperty,
    DataProperty,
    /// Plain `rdf:Property` — what schema.org declares its properties as. Not an
    /// annotation property; simply untyped as to object-vs-datatype.
    Property,
    AnnotationProperty,
}

impl Kind {
    pub fn is_property(self) -> bool {
        self != Kind::Class
    }
    pub fn iri(self) -> &'static str {
        match self {
            Kind::Class => C_OWL_CLASS,
            Kind::ObjectProperty => C_OBJ_PROP,
            Kind::DataProperty => C_DATA_PROP,
            Kind::Property => C_RDF_PROPERTY,
            Kind::AnnotationProperty => C_ANN_PROP,
        }
    }
}

#[derive(Default)]
pub struct Graph {
    iris: Vec<Box<str>>,
    lookup: HashMap<u64, Vec<Id>>,
    pub kind: Vec<Option<Kind>>,
    pub label: Vec<Option<Box<str>>>,
    pub definition: Vec<Option<Box<str>>>,
    pub synonyms: Vec<Vec<Box<str>>>,
    /// `rdfs:subClassOf` for classes ∪ `rdfs:subPropertyOf` for properties — one upward
    /// walk serves both.
    pub sup: Vec<Vec<Id>>,
    pub domain: Vec<Vec<Id>>,
    pub range: Vec<Vec<Id>>,
    pub disjoint: BTreeSet<(Id, Id)>,
    pub equivalent: BTreeSet<(Id, Id)>,
    pub inverse: Vec<(Id, Id)>,

    // Build-only scaffolding for `disjointWith [ unionOf (…) ]`; dropped after decomposing.
    union_of: HashMap<Id, Id>,
    first: HashMap<Id, Id>,
    rest: HashMap<Id, Id>,
    raw_disjoint: Vec<(Id, Id)>,
}

fn hash_of(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

impl Graph {
    /// Interning is where IRIs are canonicalised — see [`canonical_iri`]. Doing it here
    /// rather than per source means nothing reaches the graph un-normalised, including
    /// ontologies a consumer loads at runtime.
    pub fn intern(&mut self, s: &str) -> Id {
        let s = &*canonical_iri(s);
        let h = hash_of(s);
        if let Some(c) = self.lookup.get(&h) {
            for &id in c {
                if &*self.iris[id as usize] == s {
                    return id;
                }
            }
        }
        let id = self.iris.len() as Id;
        self.iris.push(s.into());
        self.kind.push(None);
        self.label.push(None);
        self.definition.push(None);
        self.synonyms.push(Vec::new());
        self.sup.push(Vec::new());
        self.domain.push(Vec::new());
        self.range.push(Vec::new());
        self.lookup.entry(h).or_default().push(id);
        id
    }

    /// Canonicalises before looking up, or a caller passing the pre-migration spelling
    /// would miss a term that is present under its canonical one.
    pub fn id_of(&self, s: &str) -> Option<Id> {
        let s = &*canonical_iri(s);
        self.lookup.get(&hash_of(s))?.iter().copied().find(|&i| &*self.iris[i as usize] == s)
    }

    pub fn iri(&self, id: Id) -> &str {
        &self.iris[id as usize]
    }

    fn len(&self) -> usize {
        self.iris.len()
    }

    /// Blank nodes are interned so the `unionOf` list can be walked, but they are
    /// scaffolding, not terms — and some sources type them `owl:Class`, so they would
    /// otherwise be emitted as `<_:b0>`, which is not even valid Turtle.
    pub fn declared(&self) -> impl Iterator<Item = Id> + '_ {
        (0..self.iris.len() as Id).filter(|&i| self.kind[i as usize].is_some() && !self.is_blank(i))
    }

    /// Both counts run over `declared()`, not over `kind` directly, so the numbers in
    /// `metadata.json` are the numbers of terms in the file. Scanning `kind` also counts
    /// blank nodes, which are never emitted: SKOS types one `owl:Class`, and metadata
    /// advertised 5 classes for a file containing 4.
    pub fn classes(&self) -> usize {
        self.declared().filter(|&i| self.kind[i as usize] == Some(Kind::Class)).count()
    }

    pub fn properties(&self) -> usize {
        self.declared().filter(|&i| self.kind[i as usize].is_some_and(|k| k.is_property())).count()
    }

    pub fn is_blank(&self, id: Id) -> bool {
        self.iri(id).starts_with("_:")
    }

    /// Stream-parse one file into the graph. Loading into a `Store` first would build a
    /// fully-indexed copy just to iterate it once — 642 MB peak for KBpedia against a
    /// 113 MB steady state.
    pub fn absorb_file(&mut self, path: &Path) -> Result<usize> {
        let fmt = match path.extension().and_then(|e| e.to_str()) {
            Some("nt") => RdfFormat::NTriples,
            Some("owl" | "rdf") => RdfFormat::RdfXml,
            _ => RdfFormat::Turtle,
        };
        let file = std::io::BufReader::new(
            std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?,
        );
        self.absorb_reader(file, fmt).with_context(|| format!("parse {}", path.display()))
    }

    pub fn absorb_bytes(&mut self, bytes: &[u8], fmt: RdfFormat) -> Result<usize> {
        self.absorb_reader(bytes, fmt)
    }

    /// Streaming variant — no decompressed copy of the file is ever held.
    pub fn absorb_reader(&mut self, r: impl std::io::Read, fmt: RdfFormat) -> Result<usize> {
        let mut n = 0usize;
        for q in RdfParser::from_format(fmt).for_reader(r) {
            let q = q?;
            n += 1;
            let s = match &q.subject {
                NamedOrBlankNode::NamedNode(x) => self.intern(x.as_str()),
                NamedOrBlankNode::BlankNode(b) => self.intern(&format!("_:{}", b.as_str())),
            };
            let p = q.predicate.as_str().to_string();
            match &q.object {
                Term::Literal(l) => {
                    let v = l.value().to_string();
                    self.absorb_literal(s, &p, &v)
                }
                Term::NamedNode(x) => {
                    let o = self.intern(x.as_str());
                    self.absorb(s, &p, o, true)
                }
                Term::BlankNode(b) => {
                    let o = self.intern(&format!("_:{}", b.as_str()));
                    self.absorb(s, &p, o, false)
                }
            }
        }
        Ok(n)
    }

    fn absorb(&mut self, s: Id, p: &str, o: Id, named: bool) {
        if p == P_TYPE {
            let oi = self.iri(o);
            let k = match oi {
                C_OWL_CLASS | C_RDFS_CLASS => Some(Kind::Class),
                C_OBJ_PROP => Some(Kind::ObjectProperty),
                C_DATA_PROP => Some(Kind::DataProperty),
                C_ANN_PROP => Some(Kind::AnnotationProperty),
                C_RDF_PROPERTY => Some(Kind::Property),
                _ => None,
            };
            if let Some(k) = k {
                // A specific declaration wins over a generic rdf:Property.
                match self.kind[s as usize] {
                    Some(Kind::Property | Kind::AnnotationProperty) | None => {
                        self.kind[s as usize] = Some(k)
                    }
                    _ => {}
                }
            }
            return;
        }
        match p {
            P_SUBCLASS | P_SUBPROP if named => self.sup[s as usize].push(o),
            P_DOMAIN | P_DOMAIN_INCLUDES if named => self.domain[s as usize].push(o),
            P_RANGE | P_RANGE_INCLUDES if named => self.range[s as usize].push(o),
            P_INVERSE if named => self.inverse.push((s, o)),
            P_DISJOINT => self.raw_disjoint.push((s, o)),
            P_EQ_CLASS | P_EQ_PROP if named => {
                self.equivalent.insert(ordered(s, o));
            }
            P_UNION => {
                self.union_of.insert(s, o);
            }
            P_FIRST => {
                self.first.insert(s, o);
            }
            P_REST => {
                self.rest.insert(s, o);
            }
            _ => {}
        }
    }

    fn absorb_literal(&mut self, s: Id, p: &str, v: &str) {
        let i = s as usize;
        match p {
            P_LABEL => self.label[i] = Some(v.into()),
            // rdfs:label wins; SKOS vocabularies carry only prefLabel.
            P_PREF_LABEL if self.label[i].is_none() => self.label[i] = Some(v.into()),
            P_ALT_LABEL => self.synonyms[i].push(v.into()),
            P_COMMENT | P_DEFINITION if self.definition[i].is_none() => {
                self.definition[i] = Some(v.into())
            }
            _ => {}
        }
    }

    /// `X disjointWith [ owl:unionOf (A B C) ]` ≡ `X⊥A, X⊥B, X⊥C`.
    ///
    /// **The highest-value step in the pipeline and the easiest to get wrong.** KBpedia
    /// states all 54 of its disjointness axioms this way; a reader that only accepts
    /// `<iri>` objects yields *zero* pairs, with no error and a plausible output file.
    /// Decomposing keeps the semantics and leaves the blank-node restrictions — which are
    /// what make a materialisation explode — out of the result.
    pub fn decompose_disjointness(&mut self) {
        let nil = self.id_of(NIL);
        for (s, o) in std::mem::take(&mut self.raw_disjoint) {
            if self.is_blank(s) {
                continue;
            }
            match self.union_of.get(&o).copied() {
                None if !self.is_blank(o) => {
                    self.disjoint.insert(ordered(s, o));
                }
                Some(head) => {
                    let mut node = Some(head);
                    let mut guard = 0;
                    while let Some(nd) = node {
                        if Some(nd) == nil || guard > 100_000 {
                            break;
                        }
                        if let Some(&m) = self.first.get(&nd)
                            && !self.is_blank(m)
                        {
                            self.disjoint.insert(ordered(s, m));
                        }
                        node = self.rest.get(&nd).copied();
                        guard += 1;
                    }
                }
                _ => {}
            }
        }
        self.union_of = HashMap::new();
        self.first = HashMap::new();
        self.rest = HashMap::new();
    }

    /// The name a term is *known by* — its label if it has one, else its IRI tail
    /// decamelised. For SKOS vocabularies the label is the real name: KBpedia's IRIs are
    /// disambiguated (`Database-Physical`) while its `prefLabel` is "database", so
    /// filtering on the IRI drops exactly the concept you were looking for.
    pub fn term_name(&self, id: Id) -> String {
        match &self.label[id as usize] {
            Some(l) => normalize(l),
            None => normalize(&decamel(local(self.iri(id)))),
        }
    }

    /// Closing upward rather than re-parenting is what keeps the interior of a taxonomy.
    /// A lexical filter judges each term alone and deleted `medical school graduate` while
    /// keeping its children, leaving orphans to be re-parented onto whatever ancestor
    /// happened to pass. Here an interior class survives because something below it does.
    /// Walks equivalence as well as `subClassOf`. Equivalence is subsumption both ways
    /// (`scm-eqc1`), so a reasoner reaches through it and a plain parent walk does not:
    /// KBpedia states `kko:Generals ≡ kko:SuperTypes`, and omitting this missed 26,544
    /// subsumptions against a materialised closure.
    /// Every term that carries an axiom — a disjointness, an equivalence, an inverse.
    ///
    /// Seeds on the same footing as any lexical selection: a term is *in* an axiom because
    /// someone decided it constrains something, which a name-based filter cannot see.
    /// Replaces a pile of force-keeps scattered through `restrict_to` and its callers.
    pub fn axiom_participants(&self) -> HashSet<Id> {
        let mut out = HashSet::new();
        for &(a, b) in self.disjoint.iter().chain(self.equivalent.iter()) {
            out.insert(a);
            out.insert(b);
        }
        for &(a, b) in &self.inverse {
            out.insert(a);
            out.insert(b);
        }
        // A term this graph does not declare has nothing to contribute; it is the far end
        // of an alignment, interned so the edge exists but defined elsewhere.
        out.retain(|&id| self.kind[id as usize].is_some() && !self.is_blank(id));
        out
    }

    pub fn disjointness_index(&self) -> HashMap<Id, Vec<Id>> {
        let mut d: HashMap<Id, Vec<Id>> = HashMap::new();
        for &(a, b) in &self.disjoint {
            d.entry(a).or_default().push(b);
            d.entry(b).or_default().push(a);
        }
        d
    }

    /// Would relating `a` to `b` put something under two classes declared disjoint?
    ///
    /// Asserting `a ⊑ b` (or `a ≡ b`) merges their ancestor sets for everything beneath
    /// `a`. If that merged set contains both ends of a disjointness axiom, every such term
    /// becomes unsatisfiable — and since the gate is *built* on those axioms, the result is
    /// not a caught error but a gate that fires on legitimate data.
    ///
    /// Used to vet imported alignments: a mapping table is a hypothesis, the disjointness
    /// we ship is what the gate runs on, so the hypothesis loses.
    pub fn union_clashes(
        &self,
        a: Id,
        b: Id,
        eq: &HashMap<Id, Vec<Id>>,
        dj: &HashMap<Id, Vec<Id>>,
    ) -> Option<(Id, Id)> {
        let anc_a = self.ancestor_closure_with(&HashSet::from([a]), eq);
        let anc_b = self.ancestor_closure_with(&HashSet::from([b]), eq);
        for &x in &anc_a {
            for &y in dj.get(&x).map(|v| &v[..]).unwrap_or(&[]) {
                if anc_b.contains(&y) {
                    return Some((x, y));
                }
            }
        }
        None
    }

    pub fn children_index(&self) -> HashMap<Id, Vec<Id>> {
        let mut c: HashMap<Id, Vec<Id>> = HashMap::new();
        for id in self.declared() {
            for &p in &self.sup[id as usize] {
                c.entry(p).or_default().push(id);
            }
        }
        c
    }

    /// Follows equivalence as well as `subClassOf`: equivalence is subsumption both ways,
    /// so a peer sits at the same level and everything beneath it is beneath `n` too.
    /// Walking only the hierarchy left 152 unsatisfiable terms past a vetted build.
    pub fn descendants(
        &self,
        seeds: &[Id],
        children: &HashMap<Id, Vec<Id>>,
        eq: &HashMap<Id, Vec<Id>>,
    ) -> HashSet<Id> {
        let mut out: HashSet<Id> = seeds.iter().copied().collect();
        let mut q: Vec<Id> = seeds.to_vec();
        while let Some(n) = q.pop() {
            let below = children.get(&n).map(|v| &v[..]).unwrap_or(&[]);
            let peers = eq.get(&n).map(|v| &v[..]).unwrap_or(&[]);
            for &c in below.iter().chain(peers) {
                if out.insert(c) {
                    q.push(c);
                }
            }
        }
        out
    }

    /// Would asserting `x ⊑ y` (or `x ≡ y`) make some term unsatisfiable?
    ///
    /// Comparing only `x`'s ancestors against `y`'s is **not** sufficient, and assuming it
    /// was left 2,138 unsatisfiable terms in a build that reported every alignment vetted.
    /// The edge gives every *descendant* of `x` the ancestors of `y`, and a descendant
    /// reaches ancestors through its other parents that `x` never had — so the clash
    /// appears one level down, where the check was not looking.
    ///
    /// Sound formulation: let `P` be the disjointness partners of the ancestors `y`
    /// contributes. The edge is unsafe exactly when something below `x` is also below
    /// something in `P` — two downward walks rather than an ancestor set per descendant.
    pub fn edge_is_safe(
        &self,
        x: Id,
        y: Id,
        eq: &HashMap<Id, Vec<Id>>,
        dj: &HashMap<Id, Vec<Id>>,
        children: &HashMap<Id, Vec<Id>>,
    ) -> bool {
        let gained = self.ancestor_closure_with(&HashSet::from([y]), eq);
        let partners: Vec<Id> =
            gained.iter().flat_map(|a| dj.get(a).map(|v| &v[..]).unwrap_or(&[])).copied().collect();
        if partners.is_empty() {
            return true;
        }
        let forbidden = self.descendants(&partners, children, eq);
        let below_x = self.descendants(&[x], children, eq);
        below_x.is_disjoint(&forbidden)
    }

    /// The full reasoning scope of a seed set: ancestors, equivalents, and the partners of
    /// any axiom they touch — **with those partners' own ancestors**.
    ///
    /// This is the one closure. `cax-dw` fires on two type *chains*, so a disjointness
    /// partner pulled in without its ancestors yields a scope that looks complete and
    /// cannot contradict anything; that was a real bug. Build-time filtering and run-time
    /// projection both call this, so they cannot drift.
    pub fn closure(&self, seeds: &HashSet<Id>) -> HashSet<Id> {
        let mut scope = self.ancestor_closure(seeds);
        let mut partners: HashSet<Id> = HashSet::new();
        for &(a, b) in self.disjoint.iter().chain(self.equivalent.iter()) {
            if scope.contains(&a) {
                partners.insert(b);
            }
            if scope.contains(&b) {
                partners.insert(a);
            }
        }
        scope.extend(self.ancestor_closure(&partners));
        scope
    }

    /// Adjacency for `owl:equivalentClass`, both directions.
    ///
    /// Build it once when walking many seeds. `ancestor_closure` builds it per call, which
    /// is free for a projection (one call) and quadratic for an exhaustive sweep: once the
    /// alignment tables landed this went from 204 equivalences to 830, and rebuilding it
    /// 30,443 times took the full-catalogue walk from 616 ms to 1,814 ms. Deliberately not
    /// cached on `self` — `restrict_to` rewrites `equivalent`, and a stale index would be
    /// wrong in exactly the silent way everything else here is guarded against.
    pub fn equivalence_index(&self) -> HashMap<Id, Vec<Id>> {
        let mut eq: HashMap<Id, Vec<Id>> = HashMap::new();
        for &(a, b) in &self.equivalent {
            eq.entry(a).or_default().push(b);
            eq.entry(b).or_default().push(a);
        }
        eq
    }

    pub fn ancestor_closure(&self, seeds: &HashSet<Id>) -> HashSet<Id> {
        self.ancestor_closure_with(seeds, &self.equivalence_index())
    }

    /// As [`Self::ancestor_closure`], reusing an index from [`Self::equivalence_index`].
    pub fn ancestor_closure_with(
        &self,
        seeds: &HashSet<Id>,
        eq: &HashMap<Id, Vec<Id>>,
    ) -> HashSet<Id> {
        let mut out: HashSet<Id> = seeds.clone();
        let mut queue: Vec<Id> = seeds.iter().copied().collect();
        while let Some(id) = queue.pop() {
            for &p in &self.sup[id as usize] {
                if out.insert(p) {
                    queue.push(p);
                }
            }
            if let Some(peers) = eq.get(&id) {
                for &p in peers {
                    if out.insert(p) {
                        queue.push(p);
                    }
                }
            }
        }
        out
    }

    /// Cut the graph down to `retain`, then drop edges implied by another path.
    ///
    /// Expects a set already closed by [`Self::closure`]. Given that, re-parenting is a
    /// no-op — every parent is present — and this reduces to transitive reduction, so the
    /// hierarchy that ships is the source's own rather than one synthesised to patch holes.
    pub fn restrict_to(&mut self, retain: HashSet<Id>) {
        // No rescuing here. Callers pass a set already closed by `closure`, which is where
        // axiom participants and their chains are decided — one place, one rule.
        let keep = retain;

        // Nearest kept ancestors, skipping dropped intermediates.
        let mut memo: HashMap<Id, Vec<Id>> = HashMap::new();
        for &c in &keep {
            let mut acc = HashSet::new();
            nearest(self, c, &keep, &mut acc, &mut HashSet::new());
            memo.insert(c, acc.into_iter().collect());
        }
        // Transitive reduction: drop c→p when p is reachable from c another way.
        let reduced = transitive_reduction(&memo);

        for id in 0..self.len() as Id {
            let i = id as usize;
            if !keep.contains(&id) {
                self.kind[i] = None;
                self.label[i] = None;
                self.definition[i] = None;
                self.synonyms[i].clear();
                self.sup[i].clear();
                self.domain[i].clear();
                self.range[i].clear();
            } else {
                self.sup[i] = reduced.get(&id).cloned().unwrap_or_default();
                self.domain[i].retain(|d| keep.contains(d));
                self.range[i].retain(|r| keep.contains(r));
            }
        }
        self.disjoint.retain(|(a, b)| keep.contains(a) && keep.contains(b));
        self.equivalent.retain(|(a, b)| keep.contains(a) && keep.contains(b));
        self.inverse.retain(|(a, b)| keep.contains(a) && keep.contains(b));
    }
}

fn nearest(g: &Graph, c: Id, keep: &HashSet<Id>, out: &mut HashSet<Id>, seen: &mut HashSet<Id>) {
    if !seen.insert(c) {
        return;
    }
    for &p in &g.sup[c as usize] {
        if keep.contains(&p) {
            out.insert(p);
        } else {
            nearest(g, p, keep, out, seen);
        }
    }
}

fn transitive_reduction(adj: &HashMap<Id, Vec<Id>>) -> HashMap<Id, Vec<Id>> {
    fn reach(adj: &HashMap<Id, Vec<Id>>, from: Id, seen: &mut HashSet<Id>) {
        for &p in adj.get(&from).into_iter().flatten() {
            if seen.insert(p) {
                reach(adj, p, seen);
            }
        }
    }
    let mut out = HashMap::with_capacity(adj.len());
    for (&c, parents) in adj {
        let mut kept = Vec::new();
        for &p in parents {
            let mut others = HashSet::new();
            for &q in parents {
                if q != p {
                    others.insert(q);
                    reach(adj, q, &mut others);
                }
            }
            if !others.contains(&p) {
                kept.push(p);
            }
        }
        kept.sort_unstable();
        kept.dedup();
        out.insert(c, kept);
    }
    out
}

/// Read a `lemma<TAB>pos-chars` index (see `wordnet` subcommand).
pub fn read_pos_index(path: &Path) -> Result<HashMap<String, String>> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let r: Box<dyn BufRead> = if path.extension().and_then(|e| e.to_str()) == Some("gz") {
        Box::new(std::io::BufReader::new(flate2::read::GzDecoder::new(f)))
    } else {
        Box::new(std::io::BufReader::new(f))
    };
    let mut out = HashMap::new();
    for line in r.lines() {
        let line = line?;
        if let Some((lemma, pos)) = line.split_once('\t') {
            out.insert(lemma.to_string(), pos.to_string());
        }
    }
    Ok(out)
}
