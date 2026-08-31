//! Guards on the build.
//!
//! This pipeline fails *silently*: a broken step emits a well-formed file with quietly
//! missing content, and nothing downstream notices until a user's memory stops catching
//! contradictions. Every test here exists because of a specific way that happened.

use std::collections::HashSet;
use std::path::Path;

use oxigraph::io::{RdfFormat, RdfParser};

use frona_ontologies::emit;
use frona_ontologies::graph::Graph;

/// A recipe stub — the emitter only reads its provenance fields for the header.
fn fixture_recipe() -> frona_ontologies::recipe::Recipe {
    toml::from_str(
        r#"
        name = "fixture"
        version = "0"
        upstream = "http://example.org/"
        license = "CC0"
        attribution = "nobody"
        "#,
    )
    .expect("fixture recipe")
}

fn fixture(name: &str) -> Graph {
    let mut g = Graph::default();
    g.absorb_file(Path::new("tests/fixtures").join(name).as_path()).expect("parse fixture");
    g.decompose_disjointness();
    g
}

/// Transposing `rdf:first` and `rdf:rest` while refactoring took KBpedia's disjointness
/// from 666 pairs to 20 — no error, no warning, just a smaller number in a file nobody
/// was reading. Decomposition must turn one `unionOf` expression of three members into
/// three pairs, and leave a plain named pair alone.
#[test]
fn union_disjointness_decomposes_to_named_pairs() {
    let g = fixture("union-disjoint.ttl");
    let pairs: HashSet<(String, String)> =
        g.disjoint.iter().map(|&(a, b)| (g.iri(a).to_string(), g.iri(b).to_string())).collect();

    let ex = |s: &str| format!("http://example.org/{s}");
    let has = |a: &str, b: &str| pairs.contains(&(ex(a), ex(b))) || pairs.contains(&(ex(b), ex(a)));

    assert!(has("Agent", "Document"), "unionOf member 1 decomposed: {pairs:?}");
    assert!(has("Agent", "Event"), "unionOf member 2 decomposed: {pairs:?}");
    assert!(has("Agent", "Place"), "unionOf member 3 decomposed: {pairs:?}");
    assert!(has("Document", "Place"), "plain named pair survives: {pairs:?}");
    assert_eq!(pairs.len(), 4, "exactly the implied pairs, no blank nodes: {pairs:?}");
}

/// No blank node may reach the output. They are what make a materialisation explode —
/// COSMO went from 1.3M closure triples to 30M purely on `unionOf`/restriction
/// definitions — so the emitted graph must contain named terms only.
#[test]
fn emitted_artifacts_contain_no_blank_nodes() {
    let g = fixture("union-disjoint.ttl");
    let (ttl, _) = emit::turtle(&g, &fixture_recipe());
    assert!(!ttl.contains("_:"), "emitted ontology leaked a blank node:\n{ttl}");
}

/// Schema.org's `domainIncludes` and `rangeIncludes` describe alternative intended
/// usages. They are advisory metadata, not RDFS constraints: lowering them to
/// `rdfs:domain`/`rdfs:range` makes every alternative conjunctive and changes the
/// vocabulary's meaning. Genuine RDFS constraints must remain strict alongside them.
#[test]
fn schema_includes_remain_advisory_in_the_emitted_artifact() {
    let input = br#"
        @prefix ex: <http://example.org/> .
        @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix schema: <https://schema.org/> .

        schema:location a rdf:Property ;
            schema:domainIncludes ex:Action, ex:Event ;
            schema:rangeIncludes ex:Place, ex:Text .

        ex:strictLocation a rdf:Property ;
            rdfs:domain ex:Person ;
            rdfs:range ex:Place .
    "#;
    let mut g = Graph::default();
    g.absorb_bytes(input, RdfFormat::Turtle).expect("parse fixture");
    let (ttl, _) = emit::turtle(&g, &fixture_recipe());
    let triples = RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(ttl.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("emitted Turtle must re-parse");
    let has = |subject: &str, predicate: &str, object: &str| {
        triples.iter().any(|triple| {
            triple.subject.to_string() == format!("<{subject}>")
                && triple.predicate.as_str() == predicate
                && triple.object.to_string() == format!("<{object}>")
        })
    };

    let location = "https://schema.org/location";
    for domain in ["http://example.org/Action", "http://example.org/Event"] {
        assert!(has(location, "https://schema.org/domainIncludes", domain), "{ttl}");
        assert!(!has(location, "http://www.w3.org/2000/01/rdf-schema#domain", domain), "{ttl}");
    }
    for range in ["http://example.org/Place", "http://example.org/Text"] {
        assert!(has(location, "https://schema.org/rangeIncludes", range), "{ttl}");
        assert!(!has(location, "http://www.w3.org/2000/01/rdf-schema#range", range), "{ttl}");
    }
    assert!(
        has(
            "http://example.org/strictLocation",
            "http://www.w3.org/2000/01/rdf-schema#domain",
            "http://example.org/Person",
        ),
        "{ttl}"
    );
    assert!(
        has(
            "http://example.org/strictLocation",
            "http://www.w3.org/2000/01/rdf-schema#range",
            "http://example.org/Place",
        ),
        "{ttl}"
    );
}

/// `metadata.json` is a published artifact — its counts must be the counts of terms in
/// the file beside it. They diverged once: `classes()` scanned type declarations directly
/// and so counted anonymous classes, which `declared()` correctly refuses to emit. SKOS
/// states `skos:member`'s range as an anonymous `owl:Class` union, and the release
/// advertised 5 classes for a file containing 4.
#[test]
fn counts_match_what_is_actually_emitted() {
    let g = fixture("union-disjoint.ttl");
    let (ttl, _) = emit::turtle(&g, &fixture_recipe());
    let emitted =
        |ty: &str| ttl.lines().filter(|l| l.trim_start().starts_with(&format!("a {ty}"))).count();
    assert_eq!(g.classes(), emitted("owl:Class"), "class count vs file:\n{ttl}");
    assert_eq!(g.properties(), emitted("owl:ObjectProperty"), "property count vs file:\n{ttl}");
}

/// The `triples` figure in `metadata.json` must be the number of triples a reader gets
/// back. It was taken from the length of each object list *before* rendering, but the
/// renderer collapses restated relations — FOAF states one `rdfs:domain` twice — so the
/// release advertised 382 triples for a file containing 380. Parse the output and count.
#[test]
fn reported_triple_count_matches_the_file() {
    let g = fixture("union-disjoint.ttl");
    let (ttl, reported) = emit::turtle(&g, &fixture_recipe());
    let parsed = RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(ttl.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("emitted Turtle must re-parse")
        .len();
    assert_eq!(reported, parsed, "reported {reported} triples, file holds {parsed}:\n{ttl}");
}

/// A lexical filter judges each name alone, so it deletes the interior of a taxonomy: it
/// dropped `medical school graduate` and `living things` while keeping their children.
/// Closing upward keeps an interior class because something below it was worth keeping.
#[test]
fn ancestor_closure_keeps_the_interior_of_the_hierarchy() {
    let g = fixture("union-disjoint.ttl");
    let person = g.id_of("http://example.org/Person").expect("Person interned");
    let agent = g.id_of("http://example.org/Agent").expect("Agent interned");
    let closed = g.ancestor_closure(&HashSet::from([person]));
    assert!(closed.contains(&agent), "a seed's parent is pulled into the subgraph");

    // A closed set makes re-parenting a no-op: the real edge survives as stated.
    let mut g2 = fixture("union-disjoint.ttl");
    g2.restrict_to(closed);
    assert!(g2.sup[person as usize].contains(&agent), "Person ⊑ Agent stated, not synthesised");
}

/// A symmetric axiom is written on the lower-sorting end, so that a pair is stated once
/// rather than twice. When the other end is not a term this file declares it never gets a
/// turn, and the axiom disappears — which is precisely the shape of a cross-vocabulary
/// alignment. All six of FOAF's were being dropped this way, including
/// `foaf:Agent ≡ dcterms:Agent`, a live bridge between two shipped vocabularies.
#[test]
fn alignments_to_undeclared_terms_survive() {
    let mut g = fixture("union-disjoint.ttl");
    // `ex:Agent` is declared; the external IRI is not, and sorts lower.
    let agent = g.id_of("http://example.org/Agent").expect("Agent interned");
    let external = g.intern("http://elsewhere.example/Actor");
    g.equivalent.insert((external, agent));
    let (ttl, _) = emit::turtle(&g, &fixture_recipe());
    assert!(
        ttl.contains("Actor"),
        "an alignment whose other end is undeclared must still be written:\n{ttl}"
    );
}

/// schema.org moved from `http:` to `https:` around 2019-20. In RDF the namespace is an
/// opaque identifier, so the two spellings are simply different terms and every alignment
/// written before the move points at nothing. FOAF 0.99 and KBpedia 2.50 are both frozen
/// on the old spelling, so this has to be reconciled here or those alignments stay dead.
#[test]
fn migrated_namespaces_canonicalise_to_one_term() {
    use frona_ontologies::rdf::canonical_iri;
    assert_eq!(canonical_iri("http://schema.org/Person"), "https://schema.org/Person");
    // Vocabularies that are canonically `http:` must not be rewritten.
    for untouched in [
        "http://purl.org/dc/terms/Agent",
        "http://xmlns.com/foaf/0.1/Person",
        "http://www.w3.org/2000/01/rdf-schema#subClassOf",
    ] {
        assert_eq!(canonical_iri(untouched), untouched, "{untouched} must not be rewritten");
    }
    // And interning both spellings must yield one id, or the graph holds a phantom twin.
    let mut g = Graph::default();
    let a = g.intern("http://schema.org/Person");
    let b = g.intern("https://schema.org/Person");
    assert_eq!(a, b, "both spellings intern to the same term");
    assert_eq!(g.id_of("http://schema.org/Person"), Some(a), "lookup canonicalises too");
}

/// A lexical filter strips abstract classes like `PhysicalObject`, and those are exactly
/// where disjointness lives — dropping them silently disarms the gate. This used to be
/// four separate rescues scattered across `restrict_to` and the callers; it is now one
/// rule, so this test exercises the rule rather than one of its patches.
#[test]
fn axiom_participants_seed_the_scope() {
    let mut g = fixture("union-disjoint.ttl");
    let person = g.id_of("http://example.org/Person").expect("Person interned");

    let mut seeds = HashSet::from([person]);
    seeds.extend(g.axiom_participants());
    let scope = g.closure(&seeds);

    g.restrict_to(scope);
    assert_eq!(g.disjoint.len(), 4, "every disjointness survives a filter that excluded it");
    let (ttl, _) = emit::turtle(&g, &fixture_recipe());
    assert!(ttl.contains("Agent"), "Agent kept because it carries an axiom");
}

/// The closure must bring a disjointness partner's **ancestors**, not just the partner.
/// `cax-dw` fires on two type chains: a partner without its chain leaves a scope that
/// looks complete and cannot contradict anything. That was a real bug in `ont project`.
#[test]
fn closure_brings_axiom_partners_with_their_chains() {
    let g = fixture("union-disjoint.ttl");
    let person = g.id_of("http://example.org/Person").unwrap();
    let agent = g.id_of("http://example.org/Agent").unwrap();
    let place = g.id_of("http://example.org/Place").unwrap();

    // Person alone: its ancestor Agent is disjoint with Place, so Place must come too.
    let scope = g.closure(&HashSet::from([person]));
    assert!(scope.contains(&agent), "ancestor reached");
    assert!(scope.contains(&place), "the ancestor's disjointness partner reached");
}

/// Transitive reduction must not disconnect a survivor from its hierarchy: when an
/// intermediate is dropped, the child re-parents to the nearest *kept* ancestor rather
/// than losing its chain.
#[test]
fn restriction_reparents_to_nearest_kept_ancestor() {
    let mut g = fixture("union-disjoint.ttl");
    let person = g.id_of("http://example.org/Person").unwrap();
    let agent = g.id_of("http://example.org/Agent").unwrap();
    g.restrict_to(HashSet::from([person, agent]));
    assert!(g.sup[person as usize].contains(&agent), "Person ⊑ Agent survives restriction");
}

/// The probe is a floor, not a score. An empty graph must report zero rather than
/// panicking or accidentally matching.
#[test]
fn coverage_probe_is_zero_on_an_empty_graph() {
    assert_eq!(frona_ontologies::probe::coverage(&Graph::default()), 0);
}

/// An alignment is safe only if nothing *beneath* its subject becomes unsatisfiable.
///
/// This took three attempts. Comparing the two endpoints' ancestor sets left **2,138**
/// unsatisfiable terms: the edge gives every *descendant* of the subject the object's
/// ancestors, and a descendant reaches other ancestors through its other parents, so the
/// clash appears a level below where the check was looking. Walking descendants by
/// `subClassOf` alone still left **152**, because equivalence is subsumption in both
/// directions and its peers sit at the same level. Both holes were silent — the build
/// reported every alignment vetted while thousands of terms were unusable.
#[test]
fn an_alignment_is_judged_by_what_lies_beneath_it() {
    let mut g = fixture("union-disjoint.ttl");
    let person = g.id_of("http://example.org/Person").unwrap();
    let place = g.id_of("http://example.org/Place").unwrap();

    // Person ⊑ Agent and Agent ⊥ Place, so nothing may put Person under Place.
    let (eq, dj, ch) = (g.equivalence_index(), g.disjointness_index(), g.children_index());
    assert!(
        !g.edge_is_safe(person, place, &eq, &dj, &ch),
        "refused: Person inherits Agent, and Agent ⊥ Place"
    );

    // The same edge from a term with no disjoint ancestry is fine — it is the descendants
    // that decide, not the endpoints.
    let fresh = g.intern("http://example.org/Unrelated");
    let (eq, dj, ch) = (g.equivalence_index(), g.disjointness_index(), g.children_index());
    assert!(g.edge_is_safe(fresh, place, &eq, &dj, &ch), "an unconstrained edge is allowed");
}

/// Blank node labels are scoped to the file that states them. KBpedia is two `.n3` files
/// absorbed into one graph, and any source that spells its blank nodes out (`_:b0`, as
/// N-Triples and `rdf:nodeID` do) shares those names with the next file in the list.
/// Interning them under one name splices the second file's `unionOf` list onto the first
/// file's `disjointWith`: real pairs vanish, wrong ones appear, and the *count* can be
/// unchanged — so `expect_disjoint_pairs`, the guard built for exactly this failure, does
/// not see it.
#[test]
fn blank_labels_are_scoped_to_the_file_that_states_them() {
    let union = |subject: &str, member: &str| {
        format!(
            "<{subject}> <http://www.w3.org/2002/07/owl#disjointWith> _:b0 .\n\
             _:b0 <http://www.w3.org/2002/07/owl#unionOf> _:l0 .\n\
             _:l0 <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> <{member}> .\n\
             _:l0 <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> \
             <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .\n"
        )
    };
    let mut g = Graph::default();
    // Two files, each with its own `_:b0` and `_:l0` — the same names, different nodes.
    g.absorb_bytes(
        union("http://a.example/A", "http://a.example/B").as_bytes(),
        RdfFormat::NTriples,
    )
    .expect("parse file one");
    g.absorb_bytes(
        union("http://b.example/X", "http://b.example/Y").as_bytes(),
        RdfFormat::NTriples,
    )
    .expect("parse file two");
    g.decompose_disjointness();

    let pairs: HashSet<(String, String)> =
        g.disjoint.iter().map(|&(a, b)| (g.iri(a).to_string(), g.iri(b).to_string())).collect();
    let has = |a: &str, b: &str| {
        pairs.contains(&(a.to_string(), b.to_string()))
            || pairs.contains(&(b.to_string(), a.to_string()))
    };
    assert!(has("http://a.example/A", "http://a.example/B"), "file one's own pair: {pairs:?}");
    assert!(has("http://b.example/X", "http://b.example/Y"), "file two's own pair: {pairs:?}");
    assert_eq!(pairs.len(), 2, "no pair crosses the two files: {pairs:?}");
}

/// A `.` is legal inside a Turtle local name but not at the end of one, where the parser
/// reads it as the end of the statement. Emitting `ns0:Corp.` produces an artifact that
/// does not re-parse, and the build never reads its own bytes back — the first thing to
/// notice would be a consumer's loader.
#[test]
fn a_local_name_ending_in_a_dot_still_emits_parseable_turtle() {
    let mut g = Graph::default();
    g.absorb_bytes(
        b"<http://example.org/Corp.> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
          <http://www.w3.org/2002/07/owl#Class> .\n",
        RdfFormat::NTriples,
    )
    .expect("parse fixture");
    let (ttl, _) = emit::turtle(&g, &fixture_recipe());
    RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(ttl.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("emitted Turtle must re-parse: {e}\n{ttl}"));
}
