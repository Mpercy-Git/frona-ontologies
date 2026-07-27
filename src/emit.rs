//! Writing release artifacts.
//!
//! One **ordinary Turtle ontology per source** — prefixed, subject-grouped, with an
//! `owl:Ontology` header carrying provenance and licence. Nothing about the output is
//! special to us: a consumer drops it in the same directory as any ontology they
//! downloaded themselves, and it reads the same way.
//!
//! Deliberately *not* split by taxonomy/labels/axioms. A consumer that separates the
//! search surface from the reasoning scope does that when it indexes, not by fetching
//! different files.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::graph::{Graph, Id};
use crate::rdf::*;
use crate::recipe::Recipe;

#[derive(Serialize)]
pub struct Artifact {
    pub name: String,
    pub bytes: u64,
    pub bytes_uncompressed: u64,
    pub triples: usize,
    pub content_sha256: String,
}

#[derive(Serialize)]
pub struct SourceMeta {
    pub name: String,
    pub version: String,
    pub upstream: String,
    pub license: String,
    pub attribution: String,
    pub classes: usize,
    pub properties: usize,
    pub disjoint_pairs: usize,
    pub artifact: Artifact,
}

#[derive(Serialize)]
pub struct Metadata {
    pub schema_version: u32,
    pub generated_at: String,
    pub sources: Vec<SourceMeta>,
}

/// Prefixes we always spell the same way, so a CURIE written into a consumer's data
/// keeps meaning the same thing across releases. Anything else gets a generated `nsN:`.
const WELL_KNOWN: &[(&str, &str)] = &[
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("schema", "https://schema.org/"),
    ("kbpedia", "http://kbpedia.org/kko/rc/"),
    ("kko", "http://kbpedia.org/ontologies/kko#"),
];

/// Namespaces always declared, because the header and every subject use them.
const ALWAYS: &[&str] = &[
    "http://www.w3.org/2002/07/owl#",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://www.w3.org/2000/01/rdf-schema#",
    "http://purl.org/dc/terms/",
    "http://www.w3.org/2004/02/skos/core#",
];

/// Turtle needs the local part to be a valid `PN_LOCAL`; when it isn't, the caller falls
/// back to writing a full IRI.
fn split_iri(iri: &str) -> Option<(&str, &str)> {
    let idx = iri.rfind(['#', '/'])? + 1;
    let (ns, local) = iri.split_at(idx);
    let first = local.chars().next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !local.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return None;
    }
    Some((ns, local))
}

pub struct Prefixes {
    map: BTreeMap<String, String>,
}

impl Prefixes {
    /// Assign prefixes over an arbitrary set of IRIs. `build` is the pipeline's caller;
    /// `diff` needs the same spelling over triples that never became a `Graph`, and the
    /// two must agree or a diff would report cosmetic changes.
    pub fn from_iris<'a>(iris: impl Iterator<Item = &'a str>) -> Self {
        let mut namespaces: BTreeSet<String> = BTreeSet::new();
        for iri in iris {
            if let Some((ns, _)) = split_iri(iri) {
                namespaces.insert(ns.to_string());
            }
        }
        Self::assign(namespaces)
    }

    fn assign(mut namespaces: BTreeSet<String>) -> Self {
        for ns in ALWAYS {
            namespaces.insert((*ns).to_string());
        }
        let mut map = BTreeMap::new();
        let mut n = 0;
        for ns in namespaces {
            let prefix = WELL_KNOWN
                .iter()
                .find(|(_, known)| *known == ns)
                .map(|(p, _)| (*p).to_string())
                .unwrap_or_else(|| {
                    let p = format!("ns{n}");
                    n += 1;
                    p
                });
            map.insert(ns, prefix);
        }
        Self { map }
    }

    fn build(g: &Graph) -> Self {
        let mut namespaces: BTreeSet<String> = BTreeSet::new();
        let note = |iri: &str, set: &mut BTreeSet<String>| {
            if let Some((ns, _)) = split_iri(iri) {
                set.insert(ns.to_string());
            }
        };
        for id in g.declared() {
            note(g.iri(id), &mut namespaces);
            for &p in
                g.sup[id as usize].iter().chain(&g.domain[id as usize]).chain(&g.range[id as usize])
            {
                note(g.iri(p), &mut namespaces);
            }
        }
        for &(a, b) in g.disjoint.iter().chain(&g.equivalent) {
            note(g.iri(a), &mut namespaces);
            note(g.iri(b), &mut namespaces);
        }
        for &(a, b) in &g.inverse {
            note(g.iri(a), &mut namespaces);
            note(g.iri(b), &mut namespaces);
        }
        Self::assign(namespaces)
    }

    pub fn term(&self, iri: &str) -> String {
        match split_iri(iri) {
            Some((ns, local)) => match self.map.get(ns) {
                Some(p) => format!("{p}:{local}"),
                None => format!("<{iri}>"),
            },
            None => format!("<{iri}>"),
        }
    }

    pub fn header(&self) -> String {
        let mut rows: Vec<(&str, &str)> =
            self.map.iter().map(|(ns, p)| (p.as_str(), ns.as_str())).collect();
        rows.sort();
        rows.iter().map(|(p, ns)| format!("@prefix {p}: <{ns}> .\n")).collect()
    }
}

/// Turtle literal escaping. Public so `diff` renders values the same way.
pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn group(pairs: impl Iterator<Item = (Id, Id)>) -> BTreeMap<Id, Vec<Id>> {
    let mut m: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
    for (a, b) in pairs {
        m.entry(a).or_default().push(b);
        m.entry(b).or_default().push(a);
    }
    for v in m.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    m
}

/// Renders an object list, and reports **how many triples it actually wrote**.
///
/// The count has to come from here rather than from `ids.len()`: sources restate the same
/// relation more than once (FOAF states `foaf:familyName rdfs:domain foaf:Person` twice),
/// this collapses the repeat, and a caller adding up input lengths then advertises a
/// triple count the file does not contain.
fn list(px: &Prefixes, g: &Graph, ids: &[Id]) -> (String, usize) {
    let mut terms: Vec<String> = ids.iter().map(|&x| px.term(g.iri(x))).collect();
    terms.sort();
    terms.dedup();
    (terms.join(", "), terms.len())
}

/// The complete ontology as Turtle, and its triple count.
pub fn turtle(g: &Graph, r: &Recipe) -> (String, usize) {
    let px = Prefixes::build(g);
    let mut out = String::with_capacity(1 << 20);
    let mut triples = 0usize;

    out.push_str(&format!(
        "# {} {}\n\
         #\n\
         # A prepared extract of {}\n\
         # Redistributed under {}.\n\
         # Attribution: {}\n\
         #\n\
         # Filtered and restructured, not a verbatim copy. The exact recipe is at\n\
         # https://github.com/fronalabs/frona-ontologies/blob/main/recipes/{}.toml\n\n",
        r.name, r.version, r.upstream, r.license, r.attribution, r.name
    ));
    out.push_str(&px.header());
    out.push('\n');

    out.push_str(&format!(
        "<https://frona.dev/ontologies/{}> a owl:Ontology ;\n    \
         owl:versionInfo \"{}\" ;\n    \
         dcterms:source <{}> ;\n    \
         dcterms:license \"{}\" ;\n    \
         dcterms:rightsHolder \"{}\" .\n\n",
        r.name,
        escape(&r.version),
        r.upstream,
        escape(&r.license),
        escape(&r.attribution)
    ));
    triples += 5;

    // Disjointness and equivalence are symmetric; emit each pair once, on whichever end
    // sorts first, so the file states the axiom rather than repeating it.
    let disjoint_by = group(g.disjoint.iter().copied());
    let equivalent_by = group(g.equivalent.iter().copied());
    let inverse_by = group(g.inverse.iter().copied());

    let mut ids: Vec<Id> = g.declared().collect();
    ids.sort_by(|&a, &b| g.iri(a).cmp(g.iri(b)));

    for id in ids {
        let i = id as usize;
        let kind = g.kind[i].expect("declared");
        let mut clauses: Vec<String> = vec![format!("a {}", px.term(kind.iri()))];
        triples += 1;

        let add = |pred: &str, targets: &[Id], clauses: &mut Vec<String>, t: &mut usize| {
            if !targets.is_empty() {
                let (rendered, written) = list(&px, g, targets);
                clauses.push(format!("{} {}", px.term(pred), rendered));
                *t += written;
            }
        };
        let rel = if kind.is_property() { P_SUBPROP } else { P_SUBCLASS };
        add(rel, &g.sup[i], &mut clauses, &mut triples);
        add(P_DOMAIN, &g.domain[i], &mut clauses, &mut triples);
        add(P_RANGE, &g.range[i], &mut clauses, &mut triples);

        // Only the lower-sorting end writes a symmetric axiom — *unless* the other end is
        // not a term this file declares, in which case it never gets a turn and the axiom
        // vanishes. That is exactly the shape of a cross-vocabulary alignment: FOAF states
        // `foaf:Agent ≡ dcterms:Agent` and `foaf:maker ≡ dcterms:creator`, and all six of
        // its alignments were being dropped because the external IRI sorted lower.
        let emitted_elsewhere =
            |o: Id| g.kind[o as usize].is_some() && !g.is_blank(o) && g.iri(id) > g.iri(o);
        let mine = |m: &BTreeMap<Id, Vec<Id>>| -> Vec<Id> {
            m.get(&id)
                .map(|v| v.iter().copied().filter(|&o| !emitted_elsewhere(o)).collect())
                .unwrap_or_default()
        };
        add(P_DISJOINT, &mine(&disjoint_by), &mut clauses, &mut triples);
        let eq = if kind.is_property() { P_EQ_PROP } else { P_EQ_CLASS };
        add(eq, &mine(&equivalent_by), &mut clauses, &mut triples);
        add(P_INVERSE, &mine(&inverse_by), &mut clauses, &mut triples);

        if let Some(l) = &g.label[i] {
            clauses.push(format!("{} \"{}\"", px.term(P_PREF_LABEL), escape(l)));
            triples += 1;
        }
        if !g.synonyms[i].is_empty() {
            let alts: Vec<String> =
                g.synonyms[i].iter().map(|s| format!("\"{}\"", escape(s))).collect();
            clauses.push(format!("{} {}", px.term(P_ALT_LABEL), alts.join(", ")));
            triples += g.synonyms[i].len();
        }
        if let Some(d) = &g.definition[i] {
            clauses.push(format!("{} \"{}\"", px.term(P_DEFINITION), escape(d)));
            triples += 1;
        }

        out.push_str(&px.term(g.iri(id)));
        out.push('\n');
        for (n, c) in clauses.iter().enumerate() {
            out.push_str("    ");
            out.push_str(c);
            out.push_str(if n + 1 == clauses.len() { " .\n" } else { " ;\n" });
        }
        out.push('\n');
    }
    (out, triples)
}

/// Write the ontology to `dir/<name>.ttl.gz`.
///
/// `content_sha256` is over the **uncompressed** bytes: gzip embeds a timestamp, so
/// hashing the archive would report a change on every run even when nothing moved.
pub fn write_ttl(dir: &Path, name: &str, content: &str, triples: usize) -> Result<Artifact> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{name}.ttl.gz"));
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(content.as_bytes())?;
    let gz = enc.finish()?;
    std::fs::write(&path, &gz).with_context(|| format!("write {}", path.display()))?;
    Ok(Artifact {
        name: format!("{name}.ttl.gz"),
        bytes: gz.len() as u64,
        bytes_uncompressed: content.len() as u64,
        triples,
        content_sha256: hex(&Sha256::digest(content.as_bytes())),
    })
}

/// Plain gzip write, for the vendored WordNet index — not an ontology artifact.
pub fn write_gz_bytes(path: &Path, content: &str) -> Result<u64> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(content.as_bytes())?;
    let gz = enc.finish()?;
    std::fs::write(path, &gz).with_context(|| format!("write {}", path.display()))?;
    Ok(gz.len() as u64)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The attribution CC-BY obliges us to ship. Not optional, and it has to reach whatever
/// consumes these files — a release without it is a licence violation.
pub fn notice(sources: &[SourceMeta], credits: &[(String, String, String)]) -> String {
    let mut s = String::from(
        "This distribution contains derived works of the ontologies listed below.\n\
         Each is redistributed under its own licence, with attribution as required.\n\n",
    );
    for src in sources {
        s.push_str(&format!(
            "── {} {}\n   upstream:    {}\n   licence:     {}\n   attribution: {}\n\n",
            src.name, src.version, src.upstream, src.license, src.attribution
        ));
    }
    if !credits.is_empty() {
        s.push_str("\nShaped by, but not redistributed here:\n\n");
        for (name, license, attribution) in credits {
            s.push_str(&format!(
                "  {name}\n    Licence:     {license}\n    Attribution: {attribution}\n\n"
            ));
        }
    }
    s.push_str(
        "Artifacts are filtered and restructured, not verbatim copies. See the recipe\n\
         for each source in `recipes/` for exactly what was done.\n",
    );
    s
}

pub fn write_metadata(dir: &Path, meta: &Metadata) -> Result<()> {
    let json = serde_json::to_string_pretty(meta)?;
    std::fs::write(dir.join("metadata.json"), json + "\n")?;
    Ok(())
}
