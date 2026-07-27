//! End-to-end: is a projection enough to reason over?
//!
//! `equivalent_to_reasoner` checks that the *walk* returns what OWL 2 RL derives, over the
//! whole catalogue. That leaves the question the runtime actually depends on unanswered:
//! the server does not reason over the catalogue, it reasons over a **cut**. If the cut is
//! missing anything, reasoning over it silently returns fewer types than the truth — a page
//! ends up under-classified and the gate never fires, with nothing anywhere reporting a
//! problem.
//!
//! `#[ignore]`d — needs a built `dist/`. Run `mise run verify:e2e`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::time::Instant;

use frona_ontologies::graph::{Graph, Id, Kind};
use oxigraph::io::{RdfFormat, RdfParser};
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};

const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SAMPLE: usize = 1000;

fn artifacts() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir("dist").expect("dist/ — run `mise run build` first") {
        let p = e.unwrap().path();
        if !p.to_string_lossy().ends_with(".ttl.gz") {
            continue;
        }
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(&std::fs::read(&p).unwrap()[..])
            .read_to_end(&mut raw)
            .unwrap();
        v.push(raw);
    }
    assert!(!v.is_empty(), "no artifacts in dist/");
    v
}

fn all_triples() -> Vec<Triple> {
    let mut out = Vec::new();
    for b in artifacts() {
        for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(&b[..]) {
            let q = q.unwrap();
            out.push(Triple::new(q.subject, q.predicate, q.object));
        }
    }
    out
}

/// Deterministic sample. A random one would make a failure unreproducible, and the whole
/// point is to be able to go and look at whatever broke.
fn sample(pool: &[Id], n: usize) -> Vec<Id> {
    let mut state: u64 = 0x5eed_1234_dead_beef;
    let mut picked = HashSet::new();
    let mut out = Vec::new();
    while out.len() < n.min(pool.len()) {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let i = (state >> 33) as usize % pool.len();
        if picked.insert(i) {
            out.push(pool[i]);
        }
    }
    out
}

fn inferred_types(closure: &[Triple], individual: &str) -> HashSet<String> {
    let ty = NamedNode::new(TYPE).unwrap();
    closure
        .iter()
        .filter(|t| {
            t.predicate == ty
                && matches!(&t.subject, NamedOrBlankNode::NamedNode(n) if n.as_str() == individual)
        })
        .filter_map(|t| match &t.object {
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "needs dist/ — run after a build"]
fn a_projection_reasons_the_same_as_the_whole_catalogue() {
    let mut g = Graph::default();
    for b in artifacts() {
        g.absorb_bytes(&b, RdfFormat::Turtle).unwrap();
    }
    g.decompose_disjointness();

    let classes: Vec<Id> =
        g.declared().filter(|&i| g.kind[i as usize] == Some(Kind::Class)).collect();
    let picked = sample(&classes, SAMPLE);
    println!("sampling {} of {} classes", picked.len(), classes.len());

    let ind = |i: usize| format!("http://example.org/probe{i}");
    let ty = NamedNode::new(TYPE).unwrap();

    // ── reference: one individual per sampled class, reasoned over everything ──
    let t0 = Instant::now();
    let mut full = all_triples();
    for (i, &c) in picked.iter().enumerate() {
        full.push(Triple::new(
            NamedNode::new(ind(i)).unwrap(),
            ty.clone(),
            NamedNode::new(g.iri(c)).unwrap(),
        ));
    }
    let mut r = reasonable::reasoner::Reasoner::new();
    r.load_triples(full);
    r.reason();
    let closure: Vec<Triple> = r.view_output().to_vec();
    let reference: Vec<HashSet<String>> =
        (0..picked.len()).map(|i| inferred_types(&closure, &ind(i))).collect();
    drop(closure);
    drop(r);
    println!("whole-catalogue reference in {} ms", t0.elapsed().as_millis());

    // Triples keyed by subject, so a cut is a lookup rather than a re-scan per class.
    let mut by_subject: HashMap<String, Vec<Triple>> = HashMap::new();
    for t in all_triples() {
        if let NamedOrBlankNode::NamedNode(n) = &t.subject {
            by_subject.entry(n.as_str().to_string()).or_default().push(t.clone());
        }
    }

    // ── each projection, reasoned alone ──
    let t1 = Instant::now();
    let mut mismatches: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for (i, &c) in picked.iter().enumerate() {
        let scope = g.closure(&HashSet::from([c]));
        let mut cut: Vec<Triple> = scope
            .iter()
            .filter_map(|&id| by_subject.get(g.iri(id)))
            .flat_map(|v| v.iter().cloned())
            .collect();
        cut.push(Triple::new(
            NamedNode::new(ind(i)).unwrap(),
            ty.clone(),
            NamedNode::new(g.iri(c)).unwrap(),
        ));

        let mut r = reasonable::reasoner::Reasoner::new();
        r.load_triples(cut);
        r.reason();
        let got = inferred_types(r.view_output(), &ind(i));

        checked += 1;
        let want = &reference[i];
        let missing: Vec<&String> = want.difference(&got).collect();
        if !missing.is_empty() && mismatches.len() < 10 {
            mismatches.push(format!(
                "{}: cut lost {} type(s), e.g. {}",
                g.term_name(c),
                missing.len(),
                missing.iter().take(3).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    println!("{checked} projections reasoned in {} ms", t1.elapsed().as_millis());

    // A comparison of empty sets proves nothing. Two of the checks in this repo passed
    // vacuously before anyone noticed, so state the evidence.
    let total: usize = reference.iter().map(|s| s.len()).sum();
    let empties = reference.iter().filter(|s| s.is_empty()).count();
    println!(
        "compared {total} inferred types ({:.1} per class); {empties} class(es) inferred none",
        total as f64 / picked.len() as f64
    );
    assert!(total > picked.len() * 3, "too few inferred types — the comparison is near-vacuous");

    assert!(
        mismatches.is_empty(),
        "{} projection(s) entailed less than the whole catalogue:\n  {}",
        mismatches.len(),
        mismatches.join("\n  ")
    );
}

/// The same question for the **gate**, which the type check above does not reach.
///
/// Dropping axiom partners from the closure does not lose a single inferred type — verified
/// by mutation — because partners exist to make a contradiction *detectable*, not to
/// classify. A projection can therefore be complete for typing and useless for the gate,
/// which is the failure that matters: a page with contradictory types sails through.
///
/// So: for each sampled class, find a class it clashes with, assert an individual of both,
/// and require `cax-dw` to fire over the cut exactly as it does over everything.
#[test]
#[ignore = "needs dist/ — run after a build"]
fn a_projection_catches_the_same_clashes() {
    let mut g = Graph::default();
    for b in artifacts() {
        g.absorb_bytes(&b, RdfFormat::Turtle).unwrap();
    }
    g.decompose_disjointness();

    let classes: Vec<Id> =
        g.declared().filter(|&i| g.kind[i as usize] == Some(Kind::Class)).collect();
    let (eq, dj) = (g.equivalence_index(), g.disjointness_index());

    // Pair each sampled class with something it genuinely contradicts: a disjointness
    // partner of one of its ancestors.
    let mut pairs: Vec<(Id, Id)> = Vec::new();
    for &c in sample(&classes, SAMPLE).iter() {
        let anc = g.ancestor_closure_with(&HashSet::from([c]), &eq);
        if let Some(partner) = anc.iter().find_map(|a| dj.get(a).and_then(|v| v.first()).copied()) {
            pairs.push((c, partner));
        }
    }
    assert!(pairs.len() > 100, "only {} clashing pairs found — too few to trust", pairs.len());
    println!("{} clashing pairs", pairs.len());

    let ind = |i: usize| format!("http://example.org/clash{i}");
    let ty = NamedNode::new(TYPE).unwrap();
    let fires = |diags: &[String], i: usize| {
        let tag = ind(i);
        diags.iter().any(|m| {
            m.match_indices(&tag).any(|(at, _)| {
                m[at + tag.len()..].chars().next().is_none_or(|ch| !ch.is_ascii_digit())
            })
        })
    };

    // Reference: everything at once.
    let mut full = all_triples();
    for (i, &(c, d)) in pairs.iter().enumerate() {
        for t in [c, d] {
            full.push(Triple::new(
                NamedNode::new(ind(i)).unwrap(),
                ty.clone(),
                NamedNode::new(g.iri(t)).unwrap(),
            ));
        }
    }
    let mut r = reasonable::reasoner::Reasoner::new();
    r.load_triples(full);
    r.reason();
    let full_diags: Vec<String> = r.diagnostics().iter().map(|d| d.message().to_string()).collect();
    let reference: Vec<bool> = (0..pairs.len()).map(|i| fires(&full_diags, i)).collect();
    drop(r);
    let n_fire = reference.iter().filter(|&&b| b).count();
    println!("whole catalogue: {n_fire}/{} pairs flagged", pairs.len());
    assert!(n_fire > 0, "no clash fired even over the whole catalogue — the check is vacuous");

    let mut by_subject: HashMap<String, Vec<Triple>> = HashMap::new();
    for t in all_triples() {
        if let NamedOrBlankNode::NamedNode(n) = &t.subject {
            by_subject.entry(n.as_str().to_string()).or_default().push(t.clone());
        }
    }

    let mut missed: Vec<String> = Vec::new();
    for (i, &(c, d)) in pairs.iter().enumerate() {
        if !reference[i] {
            continue;
        }
        let scope = g.closure(&HashSet::from([c, d]));
        let mut cut: Vec<Triple> = scope
            .iter()
            .filter_map(|&id| by_subject.get(g.iri(id)))
            .flat_map(|v| v.iter().cloned())
            .collect();
        for t in [c, d] {
            cut.push(Triple::new(
                NamedNode::new(ind(i)).unwrap(),
                ty.clone(),
                NamedNode::new(g.iri(t)).unwrap(),
            ));
        }
        let mut r = reasonable::reasoner::Reasoner::new();
        r.load_triples(cut);
        r.reason();
        let diags: Vec<String> = r.diagnostics().iter().map(|x| x.message().to_string()).collect();
        if !fires(&diags, i) && missed.len() < 10 {
            missed.push(format!("{} + {}", g.term_name(c), g.term_name(d)));
        }
    }
    assert!(
        missed.is_empty(),
        "{} clash(es) the whole catalogue catches and the projection does not:\n  {}",
        missed.len(),
        missed.join("\n  ")
    );
}
