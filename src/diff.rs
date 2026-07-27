//! `ont diff <source> <term>` — what the build did to one term.
//!
//! The pipeline drops roughly a third of every source, and until now the only way to see
//! *what* was a full-file comparison. Two real gaps went unnoticed that way: Dublin Core
//! states domain and range as `dcam:domainIncludes`, and schema.org states inverses as
//! `schema:inverseOf`, neither of which the projection matches — so both vanish with no
//! error and a plausible-looking artifact. Looking at one term shows it immediately.
//!
//! Output is Turtle with a diff marker in column 0, so stripping the markers leaves a
//! readable block of the term.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use oxigraph::io::{RdfFormat, RdfParser};
use oxrdf::{NamedOrBlankNode, Term};

use crate::emit::Prefixes;
use crate::recipe::Recipe;

/// One predicate-object pair. Compared on the full value; only the rendering truncates.
type Po = (String, String);

/// Long definitions make a diff unreadable, but truncating before comparison would call
/// two different values equal. Compare on the whole string, shorten only to print.
const MAX_LITERAL: usize = 150;

fn fmt_for(path: &Path) -> RdfFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some("nt") => RdfFormat::NTriples,
        Some("owl" | "rdf") => RdfFormat::RdfXml,
        _ => RdfFormat::Turtle,
    }
}

fn read_maybe_gz(path: &Path) -> Result<Vec<u8>> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if path.extension().and_then(|e| e.to_str()) == Some("gz") {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&raw[..])
            .read_to_end(&mut out)
            .with_context(|| format!("gunzip {}", path.display()))?;
        Ok(out)
    } else {
        Ok(raw)
    }
}

fn local_name(iri: &str) -> &str {
    match iri.rfind(['#', '/']) {
        Some(i) => &iri[i + 1..],
        None => iri,
    }
}

fn is_label(p: &str) -> bool {
    p.ends_with("rdf-schema#label") || p.ends_with("skos/core#prefLabel")
}

/// Everything one file says about the subjects we care about.
#[derive(Default)]
struct Facts {
    /// subject IRI → its predicate-object pairs
    by_subject: BTreeMap<String, BTreeSet<Po>>,
    /// lowercased label → subject IRIs, for resolving a term by its label
    by_label: BTreeMap<String, BTreeSet<String>>,
    triples: usize,
}

/// Scan one file, keeping statements about subjects the matcher accepts.
fn scan(facts: &mut Facts, path: &Path, keep: &dyn Fn(&str) -> bool) -> Result<()> {
    let bytes = read_maybe_gz(path)?;
    for q in RdfParser::from_format(fmt_for(path)).for_reader(&bytes[..]) {
        let q = q.with_context(|| format!("parse {}", path.display()))?;
        let NamedOrBlankNode::NamedNode(s) = &q.subject else { continue };
        let s = s.as_str();
        let p = q.predicate.as_str();

        if is_label(p)
            && let Term::Literal(l) = &q.object
        {
            facts.by_label.entry(l.value().to_lowercase()).or_default().insert(s.to_string());
        }
        if !keep(s) {
            continue;
        }
        let o = match &q.object {
            Term::NamedNode(x) => Rendered::Iri(x.as_str().to_string()),
            Term::BlankNode(_) => Rendered::Blank,
            Term::Literal(l) => Rendered::Literal(l.value().to_string()),
        };
        facts.triples += 1;
        facts.by_subject.entry(s.to_string()).or_default().insert((p.to_string(), o.encode()));
    }
    Ok(())
}

/// Objects are stored pre-encoded so the two sides compare as plain strings, with the
/// kind kept as a tag so rendering can prefix IRIs and quote literals.
enum Rendered {
    Iri(String),
    Blank,
    Literal(String),
}

impl Rendered {
    fn encode(&self) -> String {
        match self {
            Rendered::Iri(s) => format!("I{s}"),
            Rendered::Blank => "B".to_string(),
            Rendered::Literal(s) => format!("L{s}"),
        }
    }
}

fn render_object(encoded: &str, px: &Prefixes) -> String {
    match encoded.split_at(1) {
        ("I", iri) => px.term(iri),
        ("B", _) => "[ … ]".to_string(),
        ("L", v) => {
            let shown: String = if v.chars().count() > MAX_LITERAL {
                format!("{}…", v.chars().take(MAX_LITERAL).collect::<String>())
            } else {
                v.to_string()
            };
            format!("\"{}\"", crate::emit::escape(&shown))
        }
        _ => encoded.to_string(),
    }
}

/// Which subjects a user's term refers to. An exact IRI, else a case-insensitive local
/// name, else an exact label — a term is just as likely to be remembered as "doctor" as
/// as `Doctor-Medical`.
fn resolve(term: &str, source: &Facts, built: &Facts) -> Vec<String> {
    let mut hits: BTreeSet<String> = BTreeSet::new();
    for f in [source, built] {
        for s in f.by_subject.keys() {
            if s == term || local_name(s).eq_ignore_ascii_case(term) {
                hits.insert(s.clone());
            }
        }
    }
    if hits.is_empty() {
        let want = term.to_lowercase();
        for f in [source, built] {
            if let Some(subs) = f.by_label.get(&want) {
                hits.extend(subs.iter().cloned());
            }
        }
    }
    hits.into_iter().collect()
}

pub fn run(recipe: &Recipe, cache: &Path, dist: &Path, term: &str) -> Result<String> {
    let artifact = dist.join(format!("{}.ttl.gz", recipe.name));
    if !artifact.exists() {
        bail!("{} not built — run `mise run build` first", artifact.display());
    }
    let sources: Vec<_> = recipe.files.iter().map(|f| cache.join(&f.as_file)).collect();
    for s in &sources {
        if !s.exists() {
            bail!("{} not in the cache — run `mise run build` first", s.display());
        }
    }

    // Pass one resolves the term: match on IRI or local name while indexing labels, so a
    // term that the build *dropped entirely* still resolves against the source.
    let (mut src, mut out) = (Facts::default(), Facts::default());
    let matches_name = |s: &str| s == term || local_name(s).eq_ignore_ascii_case(term);
    for f in &sources {
        scan(&mut src, f, &matches_name)?;
    }
    scan(&mut out, &artifact, &matches_name)?;

    let subjects = resolve(term, &src, &out);
    if subjects.is_empty() {
        bail!("no term matching {term:?} in {} — tried IRI, local name and label", recipe.name);
    }

    // A label match found subjects the name filter skipped, so re-scan for exactly those.
    let known: BTreeSet<&str> = subjects.iter().map(String::as_str).collect();
    if !subjects.iter().all(|s| matches_name(s)) {
        src = Facts::default();
        out = Facts::default();
        let keep = |s: &str| known.contains(s);
        for f in &sources {
            scan(&mut src, f, &keep)?;
        }
        scan(&mut out, &artifact, &keep)?;
    }

    let empty = BTreeSet::new();
    let px = Prefixes::from_iris(subjects.iter().map(String::as_str).chain(
        subjects.iter().flat_map(|s| {
            [src.by_subject.get(s), out.by_subject.get(s)]
                .into_iter()
                .flatten()
                .flat_map(|set| set.iter())
                .flat_map(|(p, o)| [Some(p.as_str()), o.strip_prefix('I')].into_iter().flatten())
        }),
    ));

    let mut w = String::new();
    w.push_str(&format!(
        "# {} {} — upstream {} vs dist/{}.ttl.gz\n\
         #   -  dropped by the build             \
         +  added by it      (unmarked) unchanged\n\n",
        recipe.name,
        recipe.version,
        sources
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", "),
        recipe.name,
    ));
    w.push_str(&px.header());

    for s in &subjects {
        let a = src.by_subject.get(s).unwrap_or(&empty);
        let b = out.by_subject.get(s).unwrap_or(&empty);
        let kept = a.intersection(b).count();

        w.push_str(&format!(
            "\n# {} — {} upstream, {} built: {kept} unchanged, {} dropped, {} added\n",
            px.term(s),
            a.len(),
            b.len(),
            a.difference(b).count(),
            b.difference(a).count(),
        ));
        if b.is_empty() {
            w.push_str("# NOT IN THE ARTIFACT — every statement below was dropped.\n");
        }
        w.push_str(&format!("{}\n", px.term(s)));

        // Merged and sorted by predicate so a value that moved — `rdfs:comment` becoming
        // `skos:definition` — shows as adjacent - and + lines rather than pages apart.
        let mut rows: Vec<(&Po, char)> = a
            .iter()
            .map(|po| (po, if b.contains(po) { ' ' } else { '-' }))
            .chain(b.difference(a).map(|po| (po, '+')))
            .collect();
        rows.sort_by_key(|(po, _)| *po);

        for (i, ((p, o), mark)) in rows.iter().enumerate() {
            let end = if i + 1 == rows.len() { " ." } else { " ;" };
            let pred = if p.ends_with("22-rdf-syntax-ns#type") { "a".into() } else { px.term(p) };
            w.push_str(&format!("{mark}   {pred} {}{end}\n", render_object(o, &px)));
        }
    }
    Ok(w)
}
