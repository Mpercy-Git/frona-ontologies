//! Cross-checks `metadata.json` against the upstream sources and the artifacts beside it.
//!
//! Deliberately does NOT touch `Graph`: the counting in `Graph` is what is being verified,
//! so reusing it would only prove it agrees with itself. Union decomposition is
//! reimplemented here from the spec for the same reason.
//!
//! `#[ignore]`d because it needs a populated `target/cache/` and `dist/` — it verifies a build
//! that already happened. Run it with `mise run verify:artifacts` after `mise run build`.
//! This is the only check that reads what was actually published rather than what the
//! pipeline believed it wrote; both count bugs found so far were invisible to everything
//! else in `tests/`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use oxigraph::io::{RdfFormat, RdfParser};
use oxrdf::{NamedOrBlankNode, Term};

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

#[derive(Default)]
struct Counts {
    classes: HashSet<String>,
    props: HashSet<String>,
    blank_classes: usize,
    blank_props: usize,
    triples: usize,
    // scaffolding for disjointness
    disjoint_raw: Vec<(String, String)>,
    union_of: HashMap<String, String>,
    first: HashMap<String, String>,
    rest: HashMap<String, String>,
}

fn node(s: &NamedOrBlankNode) -> String {
    match s {
        NamedOrBlankNode::NamedNode(x) => x.as_str().to_string(),
        NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
    }
}

fn term(t: &Term) -> Option<String> {
    match t {
        Term::NamedNode(x) => Some(x.as_str().to_string()),
        Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
        Term::Literal(_) => None,
    }
}

fn absorb(c: &mut Counts, bytes: &[u8], fmt: RdfFormat) {
    for q in RdfParser::from_format(fmt).for_reader(bytes) {
        let q = q.expect("parse");
        c.triples += 1;
        let s = node(&q.subject);
        let p = q.predicate.as_str();
        let Some(o) = term(&q.object) else { continue };
        let blank = s.starts_with("_:");
        match p {
            _ if p == format!("{RDF}type") => match o.as_str() {
                x if x == format!("{OWL}Class") || x == format!("{RDFS}Class") => {
                    if blank {
                        c.blank_classes += 1;
                    } else {
                        c.classes.insert(s);
                    }
                }
                x if x == format!("{OWL}ObjectProperty")
                    || x == format!("{OWL}DatatypeProperty")
                    || x == format!("{OWL}AnnotationProperty")
                    || x == format!("{RDF}Property") =>
                {
                    if blank {
                        c.blank_props += 1;
                    } else {
                        c.props.insert(s);
                    }
                }
                _ => {}
            },
            _ if p == format!("{OWL}disjointWith") => c.disjoint_raw.push((s, o)),
            _ if p == format!("{OWL}unionOf") => {
                c.union_of.insert(s, o);
            }
            _ if p == format!("{RDF}first") => {
                c.first.insert(s, o);
            }
            _ if p == format!("{RDF}rest") => {
                c.rest.insert(s, o);
            }
            _ => {}
        }
    }
}

/// Reimplemented from the spec, not from `Graph`: `X ⊥ [ unionOf (A B C) ]` ≡ X⊥A, X⊥B, X⊥C.
fn decompose(c: &Counts) -> HashSet<(String, String)> {
    let members = |n: &str| -> Vec<String> {
        let Some(head) = c.union_of.get(n) else { return vec![n.to_string()] };
        let mut out = Vec::new();
        let mut cur = head.clone();
        while cur != format!("{RDF}nil") {
            if let Some(m) = c.first.get(&cur) {
                out.push(m.clone());
            }
            match c.rest.get(&cur) {
                Some(next) => cur = next.clone(),
                None => break,
            }
        }
        out
    };
    let mut pairs = HashSet::new();
    for (a, b) in &c.disjoint_raw {
        for x in members(a) {
            for y in members(b) {
                if x.starts_with("_:") || y.starts_with("_:") {
                    continue;
                }
                let (lo, hi) = if x <= y { (x.clone(), y) } else { (y, x.clone()) };
                pairs.insert((lo, hi));
            }
        }
    }
    pairs
}

fn fmt_for(p: &Path) -> RdfFormat {
    match p.extension().and_then(|e| e.to_str()) {
        Some("nt") => RdfFormat::NTriples,
        Some("owl" | "rdf") => RdfFormat::RdfXml,
        _ => RdfFormat::Turtle,
    }
}

fn read(p: &str) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

fn gunzip(p: &str) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&read(p)[..]).read_to_end(&mut out).expect("gunzip");
    out
}

const SOURCES: &[(&str, &[&str])] = &[
    ("kbpedia", &["target/cache/kko.n3", "target/cache/kbpedia_reference_concepts.n3"]),
    ("schema-org", &["target/cache/schemaorg.ttl"]),
    ("foaf", &["target/cache/foaf.rdf"]),
    ("dublincore", &["target/cache/dublincore.ttl"]),
    ("skos", &["target/cache/skos.rdf"]),
];

#[test]
#[ignore = "needs target/cache/ and dist/ — run after a build"]
fn verify_source_against_build() {
    let meta: serde_json::Value =
        serde_json::from_slice(&read("dist/metadata.json")).expect("metadata");

    println!(
        "\n{:<12} {:>26} {:>26} {:>26}",
        "", "UPSTREAM SOURCE", "EMITTED ARTIFACT", "METADATA.JSON"
    );
    println!(
        "{:<12} {:>26} {:>26} {:>26}",
        "source", "cls / prop / disj", "cls / prop / disj", "cls / prop / disj"
    );
    println!("{}", "-".repeat(94));

    let mut problems: Vec<String> = Vec::new();

    for (name, files) in SOURCES {
        // ---- upstream ----
        let mut src = Counts::default();
        for f in *files {
            let p = Path::new(f);
            absorb(&mut src, &read(f), fmt_for(p));
        }
        let src_disj = decompose(&src);

        // ---- emitted ----
        let raw = gunzip(&format!("dist/{name}.ttl.gz"));
        let mut out = Counts::default();
        absorb(&mut out, &raw, RdfFormat::Turtle);
        let out_disj = decompose(&out);

        // ---- metadata ----
        let m = meta["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == **name)
            .expect("source in metadata");
        let mc = m["classes"].as_u64().unwrap() as usize;
        let mp = m["properties"].as_u64().unwrap() as usize;
        let md = m["disjoint_pairs"].as_u64().unwrap() as usize;

        println!(
            "{:<12} {:>26} {:>26} {:>26}",
            name,
            format!("{} / {} / {}", src.classes.len(), src.props.len(), src_disj.len()),
            format!("{} / {} / {}", out.classes.len(), out.props.len(), out_disj.len()),
            format!("{mc} / {mp} / {md}"),
        );

        // ---- assertions: metadata must describe the artifact exactly ----
        if out.classes.len() != mc {
            problems
                .push(format!("{name}: metadata classes {mc} != emitted {}", out.classes.len()));
        }
        if out.props.len() != mp {
            problems
                .push(format!("{name}: metadata properties {mp} != emitted {}", out.props.len()));
        }
        if out_disj.len() != md {
            problems.push(format!("{name}: metadata disjoint {md} != emitted {}", out_disj.len()));
        }
        if out.blank_classes != 0 || out.blank_props != 0 {
            problems.push(format!(
                "{name}: artifact contains blank-node declarations ({} cls, {} prop)",
                out.blank_classes, out.blank_props
            ));
        }
        let mt = m["artifact"]["triples"].as_u64().unwrap() as usize;
        if out.triples != mt {
            problems.push(format!("{name}: metadata triples {mt} != parsed {}", out.triples));
        }
        let mb = m["artifact"]["bytes_uncompressed"].as_u64().unwrap() as usize;
        if raw.len() != mb {
            problems
                .push(format!("{name}: metadata bytes_uncompressed {mb} != actual {}", raw.len()));
        }
        let want = m["artifact"]["content_sha256"].as_str().unwrap();
        let got = {
            use sha2::Digest;
            let d = sha2::Sha256::digest(&raw);
            d.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        if got != want {
            problems
                .push(format!("{name}: content_sha256 mismatch\n    want {want}\n    got  {got}"));
        }

        // ---- source → artifact reconciliation ----
        // Triples the upstream file states more than once. Turtle output collapses them,
        // so any counter that adds up list lengths rather than what it wrote over-reports.
        let mut seen: HashMap<String, usize> = HashMap::new();
        for f in *files {
            let p = Path::new(f);
            for q in RdfParser::from_format(fmt_for(p)).for_reader(&read(f)[..]) {
                let q = q.expect("parse");
                if let Some(o) = term(&q.object) {
                    *seen
                        .entry(format!("{} {} {}", node(&q.subject), q.predicate.as_str(), o))
                        .or_default() += 1;
                }
            }
        }
        let dups: Vec<_> = seen.iter().filter(|&(_, &n)| n > 1).collect();
        if !dups.is_empty() {
            let extra: usize = dups.iter().map(|&(_, &n)| n - 1).sum();
            println!("             upstream restates {} triple(s), {extra} extra", dups.len());
            if *name != "kbpedia" {
                for (t, n) in dups.iter().take(6) {
                    println!("               x{n}  {t}");
                }
            }
        }

        let dropped: Vec<_> = src.classes.difference(&out.classes).cloned().collect();
        let added: Vec<_> = out.classes.difference(&src.classes).cloned().collect();
        println!(
            "             upstream blank-node decls: {} cls, {} prop | classes dropped {} | classes not in source {}",
            src.blank_classes,
            src.blank_props,
            dropped.len(),
            added.len()
        );
        if !added.is_empty() && *name != "kbpedia" {
            let mut a = added.clone();
            a.sort();
            println!("             NOT-IN-SOURCE: {a:?}");
        }
        if src_disj != out_disj && *name != "kbpedia" {
            let lost: Vec<_> = src_disj.difference(&out_disj).collect();
            println!("             disjoint lost vs source: {lost:?}");
        }
    }

    println!("\n{}", "=".repeat(94));
    if problems.is_empty() {
        println!("ALL METADATA CLAIMS VERIFIED AGAINST THE ARTIFACTS");
    } else {
        for p in &problems {
            println!("PROBLEM  {p}");
        }
    }
    assert!(problems.is_empty(), "{} problem(s)", problems.len());
}
