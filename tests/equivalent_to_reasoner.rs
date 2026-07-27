//! The extractor must agree with an OWL 2 RL reasoner. Exactly, not approximately.
//!
//! Walking the graph instead of materialising it is only sound while the cheap path
//! returns what the reasoner would. This is the contract: every subsumption `reasonable`
//! derives, `ancestor_closure` must reach, and every clash `cax-dw` reports, the
//! disjointness intersection must find.
//!
//! It is `#[ignore]`d because it needs a built `dist/`. Run `mise run verify:reasoner`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::time::Instant;

use frona_ontologies::graph::{Graph, Id};
use oxigraph::io::{RdfFormat, RdfParser};
use oxrdf::{NamedNode, Term, Triple};

const SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const SUBPROP: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

fn artifacts() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    let dir = std::fs::read_dir("dist").expect("dist/ — run `mise run build` first");
    for e in dir {
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

fn catalogue() -> Graph {
    let mut g = Graph::default();
    for b in artifacts() {
        g.absorb_bytes(&b, RdfFormat::Turtle).unwrap();
    }
    // Without this `g.disjoint` stays empty and every clash check silently passes.
    g.decompose_disjointness();
    g
}

fn triples() -> Vec<Triple> {
    let mut all = Vec::new();
    for b in artifacts() {
        for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(&b[..]) {
            let q = q.unwrap();
            all.push(Triple::new(q.subject, q.predicate, q.object));
        }
    }
    all
}

fn iri_of(t: &oxrdf::NamedOrBlankNode) -> String {
    t.to_string().trim_start_matches('<').trim_end_matches('>').to_string()
}

/// Every subsumption the reasoner derives must be reachable in the graph.
#[test]
#[ignore = "needs dist/ — run after a build"]
fn subsumption_matches_the_reasoner() {
    let g = catalogue();

    let t0 = Instant::now();
    let mut cheap: HashMap<&str, HashSet<&str>> = HashMap::new();
    let eq = g.equivalence_index();
    for id in g.declared() {
        let anc = g.ancestor_closure_with(&HashSet::from([id]), &eq);
        cheap.insert(g.iri(id), anc.iter().filter(|&&a| a != id).map(|&a| g.iri(a)).collect());
    }
    let cheap_ms = t0.elapsed().as_millis();

    let t1 = Instant::now();
    let mut r = reasonable::reasoner::Reasoner::new();
    r.load_triples(triples());
    r.reason();
    let reason_ms = t1.elapsed().as_millis();

    let (sub, subp) = (NamedNode::new(SUBCLASS).unwrap(), NamedNode::new(SUBPROP).unwrap());
    let mut missing: Vec<String> = Vec::new();
    let mut theirs_sco: HashSet<(String, String)> = HashSet::new();
    let mut checked = 0usize;
    for t in r.view_output().iter() {
        if t.predicate != sub && t.predicate != subp {
            continue;
        }
        let on_class = t.predicate == sub;
        let Term::NamedNode(o) = &t.object else { continue };
        let s = iri_of(&t.subject);
        // Reflexive entailments (`C ⊑ C`) carry no information and are not modelled.
        if s == o.as_str() {
            continue;
        }
        // The reasoner asserts `C ⊑ owl:Thing` for every class. True of everything, so it
        // is not stored; a reachability walk that returned it would only be noisier.
        if o.as_str() == OWL_THING {
            continue;
        }
        // Only terms the catalogue actually declares — the reasoner also speaks about
        // vocabulary IRIs (`rdfs:Resource` and friends) that are not ours to model.
        let Some(have) = cheap.get(s.as_str()) else { continue };
        checked += 1;
        if !have.contains(o.as_str()) && missing.len() < 15 {
            missing.push(format!("{s} ⊑ {}", o.as_str()));
        }
        if on_class {
            theirs_sco.insert((s.clone(), o.as_str().to_string()));
        }
    }

    // The other direction. Checking only "did the extractor find everything" would pass a
    // walk that invented subsumptions, which is the worse failure of the two. Restricted
    // to `subClassOf` on purpose: `reasonable` implements `scm-sco` but **not** `scm-spo`,
    // so it derives no transitive `subPropertyOf` at all and the extractor is legitimately
    // ahead of it there — verified in isolation on a three-property chain.
    let mut invented: Vec<String> = Vec::new();
    for (s, ancestors) in &cheap {
        let Some(id) = g.id_of(s) else { continue };
        if g.kind[id as usize].is_some_and(|k| k.is_property()) {
            continue;
        }
        for a in ancestors {
            // Same exclusions as when the reasoner's set was collected, or the asymmetry
            // itself reads as a disagreement: some artifacts do assert `C ⊑ owl:Thing`.
            if *a == OWL_THING {
                continue;
            }
            if g.id_of(a).is_some_and(|i| g.kind[i as usize].is_some_and(|k| k.is_property())) {
                continue;
            }
            if !theirs_sco.contains(&((*s).to_string(), (*a).to_string())) && invented.len() < 15 {
                invented.push(format!("{s} ⊑ {a}"));
            }
        }
    }

    println!(
        "checked {checked} derived subsumptions | reachability {cheap_ms} ms, reasoner {reason_ms} ms"
    );
    assert!(
        missing.is_empty(),
        "reachability missed {} subsumption(s) the reasoner derived:\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
    assert!(
        invented.is_empty(),
        "reachability invented {} subClassOf the reasoner does not derive:\n  {}",
        invented.len(),
        invented.join("\n  ")
    );
}

/// Every disjointness clash `cax-dw` reports must be found by intersecting ancestor sets
/// against the disjointness table — the gate has to survive without a materialisation.
#[test]
#[ignore = "needs dist/ — run after a build"]
fn disjointness_gate_matches_the_reasoner() {
    let g = catalogue();
    assert!(!g.disjoint.is_empty(), "no disjointness loaded — the test would pass vacuously");

    // Invert `sup` so descendants are a walk: a clash stated on abstract classes has to
    // still bite for a term far below them, which is the whole reason it ships.
    let mut children: HashMap<Id, Vec<Id>> = HashMap::new();
    for id in g.declared() {
        for &p in &g.sup[id as usize] {
            children.entry(p).or_default().push(id);
        }
    }
    let deepest = |root: Id| -> Option<Id> {
        let (mut seen, mut q, mut last) = (HashSet::from([root]), vec![root], None);
        while let Some(n) = q.pop() {
            if n != root {
                last = Some(n);
            }
            for &c in children.get(&n).map(|v| &v[..]).unwrap_or(&[]) {
                if seen.insert(c) {
                    q.push(c);
                }
            }
            if seen.len() > 300 {
                break;
            }
        }
        last
    };

    let mut pairs: Vec<(Id, Id)> = Vec::new();
    for &(x, y) in g.disjoint.iter().take(8) {
        pairs.push((x, y));
    }
    for &(x, y) in g.disjoint.iter().take(120) {
        if let (Some(dx), Some(dy)) = (deepest(x), deepest(y)) {
            pairs.push((dx, dy));
            if pairs.len() >= 16 {
                break;
            }
        }
    }
    // Controls: a term with one of its own ancestors can never clash.
    for id in g.declared().skip(500).take(4000) {
        if let Some(&a) = g.ancestor_closure(&HashSet::from([id])).iter().find(|&&a| a != id) {
            pairs.push((id, a));
            if pairs.len() >= 24 {
                break;
            }
        }
    }

    let dset: HashSet<(Id, Id)> = g.disjoint.iter().copied().collect();
    let t0 = Instant::now();
    let cheap: Vec<bool> = pairs
        .iter()
        .map(|&(x, y)| {
            let (ax, ay) =
                (g.ancestor_closure(&HashSet::from([x])), g.ancestor_closure(&HashSet::from([y])));
            ax.iter().any(|&p| ay.iter().any(|&q| dset.contains(&(p, q)) || dset.contains(&(q, p))))
        })
        .collect();
    let cheap_us = t0.elapsed().as_micros();

    // Ask the reasoner the same questions: one individual per pair, typed as both.
    let mut all = triples();
    let ty = NamedNode::new(TYPE).unwrap();
    for (i, &(x, y)) in pairs.iter().enumerate() {
        let ind = NamedNode::new(format!("http://example.org/i{i}")).unwrap();
        all.push(Triple::new(ind.clone(), ty.clone(), NamedNode::new(g.iri(x)).unwrap()));
        all.push(Triple::new(ind, ty.clone(), NamedNode::new(g.iri(y)).unwrap()));
    }
    let t1 = Instant::now();
    let mut r = reasonable::reasoner::Reasoner::new();
    r.load_triples(all);
    r.reason();
    let reason_ms = t1.elapsed().as_millis();
    let diags: Vec<String> = r.diagnostics().iter().map(|d| d.message().to_string()).collect();

    let flagged = |i: usize| {
        let tag = format!("http://example.org/i{i}");
        // Exact match: `i1` must not match inside `i10`.
        diags.iter().any(|m| {
            m.match_indices(&tag).any(|(at, _)| {
                m[at + tag.len()..].chars().next().is_none_or(|c| !c.is_ascii_digit())
            })
        })
    };

    let mut wrong = Vec::new();
    for (i, &(x, y)) in pairs.iter().enumerate() {
        if cheap[i] != flagged(i) {
            wrong.push(format!(
                "{} + {}: reachability={} reasoner={}",
                g.term_name(x),
                g.term_name(y),
                cheap[i],
                flagged(i)
            ));
        }
    }
    let clashes = (0..pairs.len()).filter(|&i| flagged(i)).count();
    println!(
        "{} pairs ({clashes} real clashes) | intersection {cheap_us} µs, reasoner {reason_ms} ms",
        pairs.len()
    );
    assert!(clashes > 0, "no clashes exercised — the test would pass vacuously");
    assert!(wrong.is_empty(), "gate disagreed on {}:\n  {}", wrong.len(), wrong.join("\n  "));
}
