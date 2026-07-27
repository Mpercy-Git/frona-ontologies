//! KBpedia's alignment tables — the only thing that connects the shipped vocabularies.
//!
//! `linkages/*.csv` are `s,p,o` rows where **KBpedia is always the object** and the other
//! vocabulary the subject. Three predicates appear, not one:
//!
//! | predicate | rows (schema.org) | means | absorbed as |
//! |---|---|---|---|
//! | `owl:equivalentClass` | 441 | `X ≡ Y` | equivalence, subject-side |
//! | `rdfs:subClassOf` | 335 | `X ⊑ Y` | parent edge, subject-side |
//! | `kko:superClassOf` | 69 | `X ⊐ Y` ≡ `Y ⊑ X` | parent edge, **inverted** |
//!
//! Dropping `superClassOf` as redundant is safe *inside* KBpedia, where every one is
//! mirrored by a `subClassOf` — verified. Here it is the only statement of that relation,
//! so dropping it loses 69 alignments outright.
//!
//! Rows are applied to whichever graph declares each subject, so the same file listed by
//! two recipes splits cleanly rather than shipping a triple twice. Nothing here decides
//! what is *safe* — that is [`Graph::edge_is_safe`](crate::graph::Graph::edge_is_safe),
//! which the server applies to user-supplied ontologies too.

use anyhow::{Context, Result};

use crate::graph::Graph;
use crate::rdf::ordered;

enum Row {
    Equivalent(String, String),
    SubClassOf { child: String, parent: String },
}

fn parse(bytes: &[u8]) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    let mut rdr = csv::Reader::from_reader(bytes);
    for rec in rdr.records() {
        let rec = rec.context("read linkage row")?;
        let (Some(s), Some(p), Some(o)) = (rec.get(0), rec.get(1), rec.get(2)) else {
            continue;
        };
        out.push(match p {
            "owl:equivalentClass" | "owl:equivalentProperty" => {
                Row::Equivalent(s.to_string(), o.to_string())
            }
            "rdfs:subClassOf" => Row::SubClassOf { child: s.into(), parent: o.into() },
            // `X superClassOf Y` is `Y ⊑ X`. Inverted here rather than dropped.
            "kko:superClassOf" => Row::SubClassOf { child: o.into(), parent: s.into() },
            _ => continue,
        });
    }
    Ok(out)
}

pub struct Accepted {
    pub subject: String,
    pub object: String,
    pub equivalent: bool,
}

#[derive(Default)]
pub struct Vetted {
    pub accepted: Vec<Accepted>,
    /// Rows refused because they would make some term unsatisfiable, with the reason.
    pub refused: Vec<String>,
}

/// Must see every vocabulary at once: a contradiction between schema.org and KBpedia is
/// invisible while either is built alone, so vetting per source reported nothing wrong
/// while 3,229 terms were unsatisfiable.
///
/// Rows are checked in file order against a catalogue that grows as they are accepted.
/// Refusing at the point of addition avoids attribution: a bad edge is never added, so
/// nothing has to work out afterwards which one to blame — a guess that rejected
/// `foaf:Organization ≡ kbpedia:Organization` when it was tried.
pub fn vet(cat: &mut Graph, bytes: &[u8]) -> Result<Vetted> {
    let dj = cat.disjointness_index();
    let mut children = cat.children_index();
    let mut out = Vetted::default();
    for row in parse(bytes)? {
        let (subject, a, b, equivalent) = match &row {
            Row::Equivalent(s, o) => (s.clone(), s.clone(), o.clone(), true),
            Row::SubClassOf { child, parent } => {
                (child.clone(), child.clone(), parent.clone(), false)
            }
        };
        // An alignment to something no source declares connects nothing.
        let declared = |iri: &str| cat.id_of(iri).is_some_and(|i| cat.kind[i as usize].is_some());
        if !declared(&a) || !declared(&b) {
            continue;
        }
        let (x, y) = (cat.intern(&a), cat.intern(&b));
        let eq = cat.equivalence_index();
        // Equivalence is subsumption both ways, so both directions have to be safe.
        let safe = cat.edge_is_safe(x, y, &eq, &dj, &children)
            && (!equivalent || cat.edge_is_safe(y, x, &eq, &dj, &children));
        if !safe {
            out.refused.push(format!(
                "{} ↔ {}: would make terms beneath it unsatisfiable",
                cat.term_name(x),
                cat.term_name(y),
            ));
            continue;
        }
        if equivalent {
            cat.equivalent.insert(ordered(x, y));
        } else if !cat.sup[x as usize].contains(&y) {
            cat.sup[x as usize].push(y);
            children.entry(y).or_default().push(x);
        }
        out.accepted.push(Accepted { subject, object: b, equivalent });
    }
    Ok(out)
}

pub fn apply(g: &mut Graph, accepted: &[Accepted]) -> usize {
    let mut n = 0;
    for a in accepted {
        let Some(sid) = g.id_of(&a.subject) else { continue };
        if g.kind[sid as usize].is_none() {
            continue;
        }
        let o = g.intern(&a.object);
        if a.equivalent {
            g.equivalent.insert(ordered(sid, o));
        } else if !g.sup[sid as usize].contains(&o) {
            g.sup[sid as usize].push(o);
        }
        n += 1;
    }
    n
}
