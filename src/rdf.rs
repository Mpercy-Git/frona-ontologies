//! Vocabulary constants and RDF helpers.
//!
//! The IRIs are spelled out rather than composed at use-site: these are compared
//! against on every triple of every source, and building them per call cost ~10
//! allocations per triple when this logic was prototyped (653 ms → 89 ms once fixed).

use std::borrow::Cow;
use std::collections::HashMap;

pub const P_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const P_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
pub const P_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
pub const NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";

pub const C_OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
pub const C_RDFS_CLASS: &str = "http://www.w3.org/2000/01/rdf-schema#Class";
pub const C_OBJ_PROP: &str = "http://www.w3.org/2002/07/owl#ObjectProperty";
pub const C_DATA_PROP: &str = "http://www.w3.org/2002/07/owl#DatatypeProperty";
pub const C_ANN_PROP: &str = "http://www.w3.org/2002/07/owl#AnnotationProperty";
pub const C_RDF_PROPERTY: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property";

pub const P_SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
pub const P_SUBPROP: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
pub const P_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
pub const P_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
pub const P_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
pub const P_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
pub const P_DISJOINT: &str = "http://www.w3.org/2002/07/owl#disjointWith";
pub const P_EQ_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
pub const P_EQ_PROP: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
pub const P_INVERSE: &str = "http://www.w3.org/2002/07/owl#inverseOf";
pub const P_UNION: &str = "http://www.w3.org/2002/07/owl#unionOf";

pub const P_PREF_LABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
pub const P_ALT_LABEL: &str = "http://www.w3.org/2004/02/skos/core#altLabel";
pub const P_DEFINITION: &str = "http://www.w3.org/2004/02/skos/core#definition";

/// schema.org states domain/range with its own predicates, which are deliberately
/// **not** OWL and carry no reasoning semantics. Recorded so the shape is visible.
pub const P_DOMAIN_INCLUDES: &str = "https://schema.org/domainIncludes";
pub const P_RANGE_INCLUDES: &str = "https://schema.org/rangeIncludes";

/// Namespaces whose *scheme* changed after publication, and the spelling that wins.
///
/// An RDF namespace is an opaque identifier, not a URL to dereference, so changing the
/// scheme forks every term in the vocabulary. schema.org launched on `http:` in 2011 and
/// made `https:` canonical around 2019-20; everything that aligned to it earlier froze the
/// old spelling. FOAF 0.99 (2014) and KBpedia 2.50 (Feb 2020) both did, and both are
/// frozen, so there is no upstream fix coming — the alignments are simply dead until the
/// two spellings are reconciled.
///
/// Only add an entry where the publisher treats both as the same vocabulary. schema.org
/// serves both and says so; `http://purl.org/…`, `http://xmlns.com/foaf/…` and the W3C
/// namespaces are canonically `http:` and must not be touched.
const SCHEME_MIGRATIONS: &[(&str, &str)] = &[("http://schema.org/", "https://schema.org/")];

/// Rewrite an IRI to its canonical spelling.
///
/// Applied at the point of interning rather than per source, so it covers the pipeline and
/// any consumer that loads a user-supplied ontology through the same graph — a vocabulary
/// someone drops in tomorrow gets the same treatment as the ones shipped today.
pub fn canonical_iri(iri: &str) -> Cow<'_, str> {
    for (from, to) in SCHEME_MIGRATIONS {
        if let Some(rest) = iri.strip_prefix(from) {
            return Cow::Owned(format!("{to}{rest}"));
        }
    }
    Cow::Borrowed(iri)
}

/// The identifying tail of an IRI — what a human reads as the term's name.
pub fn local(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

/// Split CamelCase so `DatabaseDesign` compares as "database design".
pub fn decamel(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for (i, c) in s.char_indices() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A name reduced to comparable words: separators collapsed, lowercased.
pub fn normalize(s: &str) -> String {
    s.replace(['_', '-'], " ").split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Naive English singularisation — enough to find a plural's lemma in WordNet.
///
/// WordNet indexes lemmas, so `animals`, `diseases` and `activities` are simply absent.
/// Without this they fall through to the unknown-word branch and survive as if they were
/// technical coinages: the filter keeps a term *because* the dictionary never heard of it.
pub fn singular(w: &str) -> Option<String> {
    if w.len() < 4 || !w.ends_with('s') {
        return None;
    }
    // `-ss` is not a plural (`suchness`), nor is `-us`/`-is` (`corpus`, `axis`).
    if w.ends_with("ss") || w.ends_with("us") || w.ends_with("is") {
        return None;
    }
    if let Some(stem) = w.strip_suffix("ies") {
        return Some(format!("{stem}y"));
    }
    for suffix in ["ches", "shes", "xes", "zes"] {
        if w.ends_with(suffix) {
            return Some(w[..w.len() - 2].to_string());
        }
    }
    Some(w[..w.len() - 1].to_string())
}

/// Does the dictionary call this a thing? A **seed** test, not a keep/drop decision — see
/// [`Graph::ancestor_closure`](crate::graph::Graph::ancestor_closure).
///
/// Requires WordNet to list the whole name as a noun, which is a deliberately narrow test:
/// it is the only rule measured that actually discriminates. Judging by the phrase's
/// *head* instead — `medical school graduate` is a kind of `graduate` — reads better and
/// is useless in practice, because every label in a class hierarchy is a noun phrase: on
/// KBpedia it admits 94% of terms against this rule's 44%.
///
/// Being narrow is only safe because it feeds a closure. On its own this rule deletes the
/// interior of the taxonomy — `living things`, `natural phenomena`, `medical school
/// graduate` — while keeping their children; the closure puts every one of them back,
/// because something below it was worth keeping.
pub fn is_thing_name(name: &str, pos: &HashMap<String, String>) -> bool {
    // A plural is absent from WordNet, which indexes lemmas. Without this step `animals`
    // and `diseases` fall through to the unknown branch and survive by accident — kept
    // *because* the dictionary never heard of them, which is the opposite of the test.
    match pos.get(name).or_else(|| singular(name).and_then(|s| pos.get(&s))) {
        Some(p) => p.contains('n'),
        // Unknown and one word: a technical coinage (`SPARQL`) worth keeping. Unknown and
        // multi-word: a compositional label, which is what this is here to remove.
        None => !name.contains(' '),
    }
}

/// Order a pair so `(a,b)` and `(b,a)` dedupe to one entry.
pub fn ordered<T: Ord + Clone>(a: T, b: T) -> (T, T) {
    if a <= b { (a, b) } else { (b, a) }
}

/// One N-Triples line for a triple of IRIs.
pub fn nt(s: &str, p: &str, o: &str) -> String {
    format!("<{s}> <{p}> <{o}> .\n")
}

/// One N-Triples line whose object is a plain literal.
pub fn nt_lit(s: &str, p: &str, o: &str) -> String {
    let esc =
        o.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r");
    format!("<{s}> <{p}> \"{esc}\" .\n")
}
