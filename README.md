# frona-ontologies

Two things that go together:

- **A sub-ontology extractor.** Give it a set of terms; it returns the slice of the
  catalogue needed to reason about them, and answers subsumption and disjointness by
  walking that graph instead of materialising it. Proven — in CI, both directions — to
  return exactly what an OWL 2 RL reasoner derives.
- **The artifacts it walks.** Prepared extracts of published ontologies, released as
  ordinary gzipped Turtle to this repo's [Releases](../../releases). Download one, drop it
  in your ontology directory, and it behaves like any ontology you fetched yourself.

The extractor exists because materialising an ontology is the thing that makes an OWL stack
unusable, and almost none of it is needed:

| question | walk the graph | materialise |
|---|---|---|
| subsumption over all 30,445 terms | **151 ms** | 1,727 ms · **1.2 GB** |
| the disjointness gate, 24 pairs | **2.2 ms** | 1,559 ms |
| subsumption within one term's scope | **203 µs** | 2 ms — *plus* the 168 ms of I/O to gather the cut, which needs the walk first |

Materialising the whole catalogue is 341,164 → **2,164,452** triples: 1.6 s and **1.2 GB**,
and the expansion factor climbs with size — 6.3× here against 2.2× for one cut. That is
what the first two rows are really against, because without a projection there is no
smaller thing to materialise.

The catalogue index is **37 MB** and loads in 214 ms. A projection allocates a set of term
ids and nothing else, so cutting one and answering questions over it does not move peak RSS
off that 37 MB.

```bash
$ ont project Doctor-Medical

catalogue  5 ontologies, 30445 declared terms, 341164 triples
           214 ms, peak RSS 37 MB
projection 161 terms (1 seed + 160 ancestors and axiom partners), 0 ms
           spans dublincore (2), foaf (7), kbpedia (127), schema-org (24)
entailed   2288 subsumptions over the cut, 203 µs, peak RSS 37 MB
           no disjointness clash among the seeds
```

## Using the extractor

`default-features = false` drops the build pipeline, leaving `graph` and `rdf` — four
dependencies (`oxigraph`, `oxrdf`, `flate2`, `anyhow`), no HTTP client, no reasoner. 238
crates become 149.

```toml
frona-ontologies = { git = "…", tag = "ont-<pinned>", default-features = false }
```

```rust
let mut g = Graph::default();
g.absorb_reader(gz(path)?, RdfFormat::Turtle)?;   // streams; never buffers the file
g.decompose_disjointness();                        // or disjointness stays empty

let scope   = g.closure(&seeds);                   // the sub-ontology
let parents = g.ancestor_closure(&seeds);          // subsumption
let safe    = g.edge_is_safe(x, y, &eq, &dj, &ch); // would this contradict the axioms?
```

IRI canonicalisation lives in that half too, so an ontology loaded at runtime is normalised
on the same terms as the ones shipped here — `http://schema.org/` and `https://schema.org/`
resolve to one term rather than two silently-unrelated ones.

## Why the answers can be trusted

Walking a graph is only a substitute for reasoning if it returns the same answers, so that
is asserted rather than assumed, at three levels:

| check | what it holds |
|---|---|
| `mise run verify:reasoner` | every one of **1,390,188** subsumptions the reasoner derives is reachable, and none is invented |
| `mise run verify:e2e` | **1,000 sampled classes**: reasoning over a *projection* infers the same types, and catches the same clashes, as reasoning over everything |
| `mise run verify:artifacts` | every count in `metadata.json` re-derived from the published bytes |

The middle one exists because the first does not cover the runtime path: the server reasons
over a **cut**, and a cut that is missing something returns fewer types with nothing
reporting a problem. Both e2e tests assert non-vacuity, and both have been mutation-tested —
disabling equivalence traversal makes them fail.

This all holds because the artifacts contain **no anonymous class expressions**: dropping
blank nodes takes property chains, `someValuesFrom`, `intersectionOf` and cardinality with
them, leaving taxonomy, disjointness and equivalence — which are reachability. Feed the
extractor an ontology that uses class expressions and the equivalence no longer holds;
`verify:reasoner` is what will say so.

## Sources

| source | version | licence | classes | properties | disjoint |
|---|---|---|---|---|---|
| KBpedia + KKO | 2.50 | CC BY 4.0 | 26,282 | 1,306 | **646** |
| schema.org | v30.0 | CC BY-SA 3.0 | 1,010 | 1,676 | 1 |
| FOAF | 0.99 | CC BY 1.0 | 15 | 68 | 4 |
| Dublin Core | 2020-01-20 | CC BY 4.0 | 22 | 55 | 0 |
| SKOS | 2009-08-18 | W3C Document | 4 | 28 | 3 |

Versions are **pinned in the recipes**. Upgrading one is a reviewed edit, not something
that happens because upstream published — see [Release cadence](#release-cadence).

They are **cross-linked**: KBpedia publishes alignment tables mapping its terms to
schema.org, Dublin Core and FOAF, and **1,190** of those are imported so a projection spans
vocabularies rather than stopping at one. **50 are refused** — imported wholesale they make
3,229 terms unsatisfiable, because KBpedia's disjointness is fine-grained and its mappings
are coarse (`schema:MedicalEntity ⊑ kko:HealthCare` puts every drug under `ActionTypes`,
which KBpedia declares disjoint from `Drugs`). Each row is vetted against the assembled
catalogue before it is accepted; `ont build` fails outright if any term ends up
unsatisfiable.

## Data shape

One Turtle ontology per source — prefixed, subject-grouped, with an `owl:Ontology`
header carrying version, source and licence. Nothing about the format is specific to us.

```
kbpedia.ttl.gz     4073 KB gz  (14.7 MB raw)     326,326 triples
schema-org.ttl.gz   168 KB gz  (790 KB raw)       13,937 triples
foaf.ttl.gz           3 KB gz                        398 triples
dublincore.ttl.gz     3 KB gz                        360 triples
skos.ttl.gz           2 KB gz                        143 triples
metadata.json                   triple counts, sizes, content_sha256, licences
NOTICE                          attribution required by CC-BY
```

Deliberately **not** split into taxonomy/labels/axioms files. Labels are ~90% of the
bytes and are only ever searched, but a consumer that separates its search surface from
its reasoning scope does that when it indexes — not by fetching different files.

A term reads as you'd expect:

```turtle
kbpedia:Doctor-Medical
    a owl:Class ;
    rdfs:subClassOf kbpedia:Licensed-Professional, kbpedia:Medic,
                    kbpedia:MedicalSchoolGraduate, kbpedia:Prescriber ;
    skos:prefLabel "doctor" ;
    skos:altLabel "MD", "physician", "medical practitioner", … ;
    skos:definition "… a person with a certain type of education in the field of
                     medicine who is professionally licensed to practice medicine." .
```

`NOTICE` is not optional — CC-BY requires attribution, and it must reach whatever image
consumes these files.

`content_sha256` is over the **uncompressed** bytes: gzip embeds a timestamp, so hashing
the archive would report a change on every run even when nothing moved.

## Downloading the artifacts

```
https://github.com/fronalabs/frona-ontologies/releases/latest/download/metadata.json
https://github.com/fronalabs/frona-ontologies/releases/download/<tag>/kbpedia.ttl.gz
```

Consume a **pinned tag**, not `latest`. Reasoning output changing because someone cut a
release is a different class of problem than a stale download; verify what you fetched
against `content_sha256`.

## Release cadence

These sources barely move:

| source | last actual change |
|---|---|
| KBpedia 2.50 | **Feb 2020** — frozen, zero upstream releases |
| schema.org | 2026-03-25, ~2–4 releases/year |
| SKOS | 2011 |
| FOAF, Dublin Core | years |

So there is **no schedule**. Releases are cut by `workflow_dispatch`, and in practice
because *a recipe changed*, not because upstream published. Nothing here notices a new
schema.org version — deliberately: an ontology upgrade silently changing how someone's
memory reasons is worse than lagging a release.

Tags are `ont-YYYYMMDDTHHMMSSZ`. Nothing is pruned; consumers pin tags that must keep
resolving.

## Running locally

[mise](https://mise.jdx.dev) pins the toolchain and carries the tasks; CI runs the same
ones, so a release is reproducible from a laptop.

```bash
mise install                        # rust 1.95.0, jq
mise run build                      # all sources → dist/
mise run build:one kbpedia          # just one
mise run test                       # the guards
mise run fmt                        # rustfmt
mise run verify                     # what CI does: fmt, clippy, guards, full build
```

Tasks for looking at what came out:

```bash
mise run inspect                            # per-source counts, sizes, licences
mise run show kbpedia Doctor-Medical        # one term, as emitted
mise run diff kbpedia Doctor-Medical        # what the build did to that term
mise run project Doctor-Medical             # cut a reasoning scope, and cost it
mise run verify:artifacts                   # re-derive every metadata number from dist/
mise run verify:reasoner                    # extractor must equal the OWL 2 RL reasoner
mise run verify:e2e                         # 1000 classes: projection vs whole catalogue
```

`diff` is the one to reach for when a term looks wrong. The build drops a third of
every source, and this shows *which* third for a single term — upstream on the left of the
marker, artifact on the right, both as Turtle. Rows are sorted by predicate so a value
that moved sits next to where it went:

```turtle
# skos:member — 7 upstream, 4 built: 3 unchanged, 4 dropped, 1 added
skos:member
-   a rdf:Property ;                          # owl:ObjectProperty won
    a owl:ObjectProperty ;
    rdfs:domain skos:Collection ;
-   rdfs:isDefinedBy ns0:core ;               # editorial, not kept
-   rdfs:range [ … ] ;                        # anonymous class, never emitted
-   rdfs:label "has member" ;
+   skos:prefLabel "has member" .             # …became this
```

It takes a local name, a full IRI or a label (`doctor` finds `kbpedia:Doctor-Medical`),
and works for terms that were dropped entirely — the whole point when you are asking why
something vanished. Dropped lines are red, added green, provenance dimmed — only when
stdout is a terminal, so a redirected diff stays plain. `--color always|never` overrides,
and `NO_COLOR` is honoured.

`project` is the extractor on the command line — see the top of this file for what it
costs. It names the axiom that fired when seeds contradict each other:

```
$ mise run project AVInfo Agents
entailed   257 subsumptions over the cut, 880 µs
           CLASH av info + agents via av info ⊥ agents
```

`--reasoner` materialises the same cut and checks the two agree, subsumption for
subsumption; `--baseline` materialises the whole catalogue for contrast (341,164 →
2,164,452 triples, 1.6 s, **1.2 GB**); `--ttl` prints the cut. Results are split by
predicate because `reasonable` implements `scm-sco` but **not** `scm-spo` — it derives no
transitive `subPropertyOf` at all, so the extractor is legitimately ahead of it there.

`verify:artifacts` re-parses the upstream sources *and* the built files with an
independent counter and asserts `metadata.json` describes what was actually emitted —
classes, properties, disjointness, triples, byte length and `content_sha256`. It is the
only check that reads what would be published rather than what the pipeline believed it
wrote, and both counting bugs found so far were invisible to everything else.

`mise tasks` lists the rest. Nothing here needs mise — the tasks wrap a plain binary,
`ont`, whose subcommands are `build`, `diff`, `project` and `wordnet`:

```bash
cargo run --release -- project Doctor-Medical --reasoner   # or: ./target/release/ont …
```

The binary is named for what it operates on rather than for `build`: `ont diff` and
`ont project` read artifacts, they do not build anything.

Downloads are cached in `target/cache/` and never re-fetched — every source is pinned in
its recipe, so a file that is present is by definition still correct. `mise run clean`
keeps that cache; `clean:all` drops it and forces a re-fetch of ~40 MB.

The seed test needs a part-of-speech lookup, so the build fetches **Open English WordNet
2024** (18 MB) and reduces it to a 662 KB `lemma → POS` index in the cache. That takes
0.4 s and is byte-identical every run, so it is derived rather than committed — its licence
then reaches `NOTICE` through the same path as an ontology's, which is the part that
matters: it is CC BY 4.0 plus the Princeton WordNet License, and both require attribution.

`ont wordnet <english-wordnet.ttl[.gz]>` regenerates the index by hand if you need it.

## How the build works

Per source, in order:

1. **Stream-parse.** Loading a file into an indexed store just to iterate it once peaked
   at 642 MB for KBpedia against a 113 MB steady state.
2. **Decompose disjointness.** `X disjointWith [ unionOf (A B C) ]` ≡ `X⊥A, X⊥B, X⊥C`.
   This is the highest-value step and the easiest to get wrong — see
   [Failure modes](#failure-modes).
3. **Seed on general knowledge and on axioms** (KBpedia only). A term seeds the subgraph
   when WordNet lists its *name* as a noun (plurals resolved to their lemma), or when it is
   a single word WordNet has never heard of — a technical coinage — **or when it
   participates in an axiom**: a disjointness, an equivalence, an alignment. The lexical
   test judges each name alone and cannot see that a term is load-bearing; axiom
   participation says so outright, and it is what keeps the gate armed. **63,180 → 22,327
   lexical + 36 axiom-bearing.**
   Matching runs on the **label**, not the IRI: KBpedia's IRIs are disambiguated
   (`Database-Physical` for "database"), so filtering on the IRI drops exactly the
   concept you wanted.
4. **Import alignments.** KBpedia publishes `linkages/*.csv` mapping its terms to
   schema.org, Dublin Core and FOAF — 932 rows, 830 absorbed. Three predicates, not one:
   `owl:equivalentClass`, `rdfs:subClassOf`, and `kko:superClassOf`, which is the inverse
   direction and is **inverted rather than dropped** — inside KBpedia every `superClassOf`
   is mirrored by a `subClassOf`, but across vocabularies it is the only statement of that
   relation. Each recipe absorbs the rows it declares the subject of, so the same file
   listed by two recipes splits cleanly instead of shipping twice.
5. **Close upward.** The subgraph is the seeds plus every ancestor of a seed —
   **22,327 → 27,586 terms.** This is what keeps the interior of the taxonomy. The filter
   judges each name alone and cannot see that `medical school graduate` is load-bearing
   for `doctor`; the closure keeps it because something below it was worth keeping.
6. **Transitively reduce.** Edges implied by another path are dropped. Re-parenting is not
   needed once the set is ancestor-closed — every parent is present, so the hierarchy is
   the source's own rather than one synthesised to patch over holes.

## Failure modes

This pipeline fails *silently* — a broken step emits a well-formed file with quietly
missing content. Two guards exist because that already happened:

- **`expect_disjoint_pairs`** in the recipe asserts the count exactly. Transposing
  `rdf:first` and `rdf:rest` during a refactor took KBpedia from 666 pairs to 20: no
  error, no warning, a plausible output file. `tests/build.rs` reproduces it on a
  fixture.
- **`expect_min_probe_hits`** asserts a floor on how many everyday concepts survive
  (`physician`, `meeting`, `bicycle`, `medication`…). Catches a filter that over-prunes.

Both fail the build rather than shipping.

## Licence

The tooling in this repo is MIT. The **artifacts are not** — each carries its upstream
licence, recorded per source in `metadata.json` and reproduced in `NOTICE`. Redistributing
them means honouring those terms, CC-BY attribution in particular.
