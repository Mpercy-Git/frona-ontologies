//! `ont project <term>…` — cut the subgraph a consumer would actually reason over.
//!
//! The artifacts are a **catalogue**: everything, searchable, never reasoned over as a
//! whole. The design cuts a **projection** per use instead: from the terms an agent
//! picked, take their ancestors and the axioms that touch them, and reason over only that.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use oxigraph::io::{RdfFormat, RdfParser};
use oxrdf::{NamedOrBlankNode, Term, Triple};

use crate::emit::Prefixes;
use crate::graph::{Graph, Id};
use crate::rdf::{P_SUBCLASS, P_SUBPROP};

const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

/// `ru_maxrss` is the high-water mark, not the current size, which is the number that
/// matters here: it captures the parse peak that a steady-state reading would miss.
fn peak_rss() -> u64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
        return 0;
    }
    // macOS reports bytes, Linux kilobytes.
    if cfg!(target_os = "macos") { u.ru_maxrss as u64 } else { u.ru_maxrss as u64 * 1024 }
}

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// Decompress **as a stream**. Reading each artifact into a `Vec` first put a 15 MB
/// buffer on the heap for KBpedia, and since `ru_maxrss` is a high-water mark that buffer
/// was then charged to whichever step happened to allocate it — gathering 33 triples
/// appeared to cost 20 MB.
fn reader(path: &Path) -> Result<impl Read> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(flate2::read::GzDecoder::new(std::io::BufReader::new(f)))
}

fn artifacts(dist: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dist)
        .with_context(|| format!("read {}", dist.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".ttl.gz"))
        .collect();
    out.sort();
    if out.is_empty() {
        bail!("no .ttl.gz in {} — run `mise run build` first", dist.display());
    }
    Ok(out)
}

fn local(iri: &str) -> &str {
    match iri.rfind(['#', '/']) {
        Some(i) => &iri[i + 1..],
        None => iri,
    }
}

pub struct Options {
    /// Cross-check against `reasonable`. Off by default: the extractor is the answer,
    /// the reasoner is the oracle it is checked against in `tests/equivalent_to_reasoner`.
    pub reasoner: bool,
    pub ttl: bool,
    /// Materialise the *whole* catalogue for contrast. It runs last in the same process,
    /// so the peak RSS it reports is cumulative.
    pub baseline: bool,
}

pub fn run(dist: &Path, terms: &[String], opts: &Options) -> Result<String> {
    let files = artifacts(dist)?;
    let mut w = String::new();
    let base_rss = peak_rss();

    let t0 = Instant::now();
    let mut g = Graph::default();
    let mut catalogue_triples = 0usize;
    // Recorded during absorb; deriving it later cost a second gunzip-and-parse of every
    // artifact just to answer "which file was this from".
    let mut origin: HashMap<Id, String> = HashMap::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().replace(".ttl.gz", "");
        catalogue_triples += g.absorb_reader(reader(f)?, RdfFormat::Turtle)?;
        for id in g.declared() {
            origin.entry(id).or_insert_with(|| name.clone());
        }
    }
    // `absorb` parks disjointness in scaffolding until this runs. Skipping it leaves
    // `g.disjoint` empty, and a projection then silently pulls in no axiom partners at
    // all — the cut looks right and cannot contradict anything.
    g.decompose_disjointness();
    let load_ms = t0.elapsed().as_millis();
    let load_rss = peak_rss();
    w.push_str(&format!(
        "catalogue   {} ontologies, {} declared terms, {catalogue_triples} triples\n\
             \x20          {load_ms} ms, peak RSS {} (+{} over baseline)\n\n",
        files.len(),
        g.declared().count(),
        mb(load_rss),
        mb(load_rss.saturating_sub(base_rss)),
    ));

    let mut seeds: HashSet<Id> = HashSet::new();
    for t in terms {
        let hits: Vec<Id> = g
            .declared()
            .filter(|&id| {
                let iri = g.iri(id);
                iri == t || local(iri).eq_ignore_ascii_case(t) || &g.term_name(id) == t
            })
            .collect();
        if hits.is_empty() {
            bail!("no term matching {t:?} in the catalogue");
        }
        seeds.extend(hits);
    }
    w.push_str("seeds\n");
    let mut listed: Vec<Id> = seeds.iter().copied().collect();
    listed.sort_by_key(|&id| g.iri(id).to_string());
    for id in &listed {
        w.push_str(&format!("           {} (\"{}\")\n", g.iri(*id), g.term_name(*id)));
    }

    let t1 = Instant::now();
    let scope = g.closure(&seeds);
    let cut_ms = t1.elapsed().as_millis();
    let pulled = scope.len() - seeds.len();
    w.push_str(&format!(
        "\nprojection  {} terms ({} seed{} + {pulled} ancestors and axiom partners), {cut_ms} ms\n",
        scope.len(),
        seeds.len(),
        if seeds.len() == 1 { "" } else { "s" },
    ));

    // A cut confined to one source means the artifacts carry no cross-vocabulary links.
    let mut spans: HashMap<&str, usize> = HashMap::new();
    for id in &scope {
        if let Some(src) = origin.get(id) {
            *spans.entry(src.as_str()).or_default() += 1;
        }
    }
    let mut spans: Vec<_> = spans.into_iter().collect();
    spans.sort();
    w.push_str(&format!(
        "           spans {}\n",
        spans.iter().map(|(n, c)| format!("{n} ({c})")).collect::<Vec<_>>().join(", ")
    ));

    // Reported explicitly: counting only attributed terms made the spans silently fail to
    // add up to the projection size, which is how a dangling alignment target hides.
    let dangling: Vec<&str> = {
        let mut v: Vec<&str> =
            scope.iter().filter(|id| !origin.contains_key(id)).map(|&id| g.iri(id)).collect();
        v.sort_unstable();
        v
    };
    if !dangling.is_empty() {
        w.push_str(&format!(
            "           {} term{} no source declares: {}\n",
            dangling.len(),
            if dangling.len() == 1 { "" } else { "s" },
            dangling.join(", ")
        ));
    }

    // `tests/equivalent_to_reasoner` is what holds this walk to the same answers an
    // OWL 2 RL engine gives.
    let t2 = Instant::now();
    // Hoisted: `ancestor_closure` rebuilds this per call, which is free for one cut and
    // quadratic over a scope.
    let eq = g.equivalence_index();
    let mut derived: HashSet<(Id, Id)> = HashSet::new();
    for &id in &scope {
        for a in g.ancestor_closure_with(&HashSet::from([id]), &eq) {
            if a != id {
                derived.insert((id, a));
            }
        }
    }
    let subsumptions = derived.len();
    let dset: HashSet<(Id, Id)> = g.disjoint.iter().copied().collect();
    let mut clashes: Vec<String> = Vec::new();
    let listed_seeds: Vec<Id> = listed.clone();
    for (i, &x) in listed_seeds.iter().enumerate() {
        for &y in &listed_seeds[i + 1..] {
            // Sorted: several axioms can explain one clash, and a `HashSet` scan named a
            // different one on each run.
            let sorted = |id| {
                let mut v: Vec<Id> =
                    g.ancestor_closure_with(&HashSet::from([id]), &eq).into_iter().collect();
                v.sort_by_key(|&i| g.iri(i).to_string());
                v
            };
            let (ax, ay) = (sorted(x), sorted(y));
            if let Some((p, q)) = ax.iter().find_map(|&p| {
                ay.iter()
                    .find(|&&q| dset.contains(&(p, q)) || dset.contains(&(q, p)))
                    .map(|&q| (p, q))
            }) {
                clashes.push(format!(
                    "{} + {} via {} ⊥ {}",
                    g.term_name(x),
                    g.term_name(y),
                    g.term_name(p),
                    g.term_name(q)
                ));
            }
        }
    }
    let walk_us = t2.elapsed().as_micros();
    w.push_str(&format!(
        "\nentailed    {subsumptions} subsumptions over the cut, {walk_us} µs, peak RSS {}\n",
        mb(peak_rss()),
    ));
    if clashes.is_empty() {
        w.push_str("           no disjointness clash among the seeds\n");
    } else {
        for c in &clashes {
            w.push_str(&format!("           CLASH {c}\n"));
        }
    }

    // Only built to hand to a reasoner or to print; the extractor never needs it.
    let mut kept: Vec<Triple> = Vec::new();
    if opts.reasoner || opts.ttl {
        let t3 = Instant::now();
        for f in &files {
            collect(reader(f)?, &g, &scope, &mut kept)?;
        }
        w.push_str(&format!(
            "\ngathered    {} triples ({:.2}% of the catalogue), {} ms re-reading the \
             artifacts, peak RSS {}\n",
            kept.len(),
            100.0 * kept.len() as f64 / catalogue_triples as f64,
            t3.elapsed().as_millis(),
            mb(peak_rss()),
        ));
    }

    if opts.reasoner {
        let n_in = kept.len();
        let t4 = Instant::now();
        let mut r = reasonable::reasoner::Reasoner::new();
        r.load_triples(kept.clone());
        r.reason();
        let out = r.view_output();
        let reason_ms = t4.elapsed().as_millis();
        w.push_str(&format!(
            "reasoner    {n_in} → {} triples ({:.1}x), {reason_ms} ms, peak RSS {}\n",
            out.len(),
            out.len() as f64 / n_in as f64,
            mb(peak_rss()),
        ));

        // The two triple counts above are not comparable — one is a subsumption set, the
        // other every triple in a materialisation. Compare the subsumptions.
        let mut theirs: HashSet<(Id, Id)> = HashSet::new();
        for t in out.iter() {
            let p = t.predicate.as_str();
            if p != P_SUBCLASS && p != P_SUBPROP {
                continue;
            }
            let Term::NamedNode(o) = &t.object else { continue };
            // `C ⊑ owl:Thing` and `C ⊑ C` hold of everything and are not modelled.
            if o.as_str() == OWL_THING {
                continue;
            }
            let subject = t.subject.to_string();
            let subject = subject.trim_start_matches('<').trim_end_matches('>');
            if subject == o.as_str() {
                continue;
            }
            if let (Some(a), Some(b)) = (g.id_of(subject), g.id_of(o.as_str()))
                && scope.contains(&a)
            {
                theirs.insert((a, b));
            }
        }
        // Split by kind. `reasonable` implements `scm-sco` but **not** `scm-spo`, so it
        // derives no transitive `subPropertyOf` at all — verified on a three-property
        // chain in isolation. Lumping the two together makes the extractor look wrong
        // where it is in fact the more complete of the two.
        let is_prop = |id: Id| g.kind[id as usize].is_some_and(|k| k.is_property());
        for (label, want_exact) in [("subClassOf", true), ("subPropertyOf", false)] {
            let pick = |set: &HashSet<(Id, Id)>| -> HashSet<(Id, Id)> {
                set.iter()
                    .copied()
                    .filter(|&(a, _)| is_prop(a) == (label == "subPropertyOf"))
                    .collect()
            };
            let (mine, theirs) = (pick(&derived), pick(&theirs));
            let missed: Vec<_> = theirs.difference(&mine).collect();
            let extra: Vec<_> = mine.difference(&theirs).collect();
            let note = if !want_exact && !extra.is_empty() {
                "  (reasonable does not implement scm-spo)"
            } else {
                ""
            };
            w.push_str(&format!(
                "           {label:<14} {} shared, {} reasoner-only, {} extractor-only{note}\n",
                theirs.intersection(&mine).count(),
                missed.len(),
                extra.len(),
            ));
            // A subsumption the reasoner derived and the extractor did not is a defect in
            // the extractor, whichever predicate it is on.
            for &&(a, b) in missed.iter().take(5) {
                w.push_str(&format!("           MISSED {} ⊑ {}\n", g.iri(a), g.iri(b)));
            }
            if want_exact {
                for &&(a, b) in extra.iter().take(5) {
                    w.push_str(&format!("           EXTRA  {} ⊑ {}\n", g.iri(a), g.iri(b)));
                }
            }
        }
    }

    if opts.baseline {
        let t4 = Instant::now();
        let mut all: Vec<Triple> = Vec::new();
        for f in &files {
            for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(reader(f)?) {
                let q = q?;
                all.push(Triple::new(q.subject, q.predicate, q.object));
            }
        }
        let n_in = all.len();
        let mut r = reasonable::reasoner::Reasoner::new();
        r.load_triples(all);
        r.reason();
        let closure = r.view_output().len();
        w.push_str(&format!(
            "baseline    whole catalogue: {n_in} → {closure} triples ({:.1}x), {} ms, \
             peak RSS {}\n",
            closure as f64 / n_in as f64,
            t4.elapsed().as_millis(),
            mb(peak_rss()),
        ));
    }

    if opts.ttl {
        let px = Prefixes::from_iris(scope.iter().map(|&id| g.iri(id)));
        w.push('\n');
        w.push_str(&px.header());
        w.push('\n');
        let mut by_subject: Vec<&Triple> = kept.iter().collect();
        by_subject.sort_by_key(|t| t.subject.to_string());
        let mut current = String::new();
        for t in by_subject {
            let s = t.subject.to_string();
            if s != current {
                if !current.is_empty() {
                    w.push_str(" .\n\n");
                }
                current = s;
                w.push_str(&px.term(&strip(&current)));
                w.push('\n');
            } else {
                w.push_str(" ;\n");
            }
            w.push_str(&format!(
                "    {} {}",
                px.term(t.predicate.as_str()),
                render(&t.object, &px)
            ));
        }
        if !current.is_empty() {
            w.push_str(" .\n");
        }
    }

    Ok(w)
}

fn strip(s: &str) -> String {
    s.trim_start_matches('<').trim_end_matches('>').to_string()
}

fn render(o: &Term, px: &Prefixes) -> String {
    match o {
        Term::NamedNode(n) => px.term(n.as_str()),
        Term::BlankNode(_) => "[]".to_string(),
        Term::Literal(l) => format!("\"{}\"", crate::emit::escape(l.value())),
    }
}

/// Keep the in-scope triples; report how many distinct in-scope subjects this file held.
fn collect(r: impl Read, g: &Graph, scope: &HashSet<Id>, out: &mut Vec<Triple>) -> Result<usize> {
    let mut seen: HashSet<Id> = HashSet::new();
    for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(r) {
        let q = q?;
        if let NamedOrBlankNode::NamedNode(s) = &q.subject
            && let Some(id) = g.id_of(s.as_str())
            && scope.contains(&id)
        {
            seen.insert(id);
            out.push(Triple::new(q.subject, q.predicate, q.object));
        }
    }
    Ok(seen.len())
}
