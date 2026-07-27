//! `ont` — turn published ontologies into small, ready-to-index release artifacts.
//!
//!   ont build [--source <name>] [--out dist] [--cache target/cache] [--recipes recipes]
//!   ont diff <source> <term> [--out dist] [--cache target/cache] [--color <when>]
//!   ont project <term>… [--reasoner] [--baseline] [--ttl] [--out dist]
//!   ont wordnet <english-wordnet.ttl> [--out data/wordnet-pos.tsv.gz]
//!
//! `build` is what CI runs. `diff` shows what the build did to one term, upstream
//! versus artifact. `wordnet` regenerates the vendored part-of-speech index and is run by
//! hand when WordNet is upgraded — the full distribution is 202 MB and has no business
//! being fetched on every CI run.
//!
//! Downloads cache under `target/` so one `cargo clean` clears everything the build owns.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use frona_ontologies::graph::{Graph, read_pos_index};
use frona_ontologies::linkage;
use frona_ontologies::rdf::is_thing_name;
use frona_ontologies::recipe::Recipe;
use frona_ontologies::term;
use frona_ontologies::{emit, probe};

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("build") => build(&args),
        Some("diff") => diff(&args),
        Some("project") => project(&args),
        Some("wordnet") => wordnet(&args),
        _ => {
            eprintln!(
                "usage:\n  \
                 ont build    [--source <name>] [--out dist] [--cache target/cache]\n  \
                 ont diff     <source> <term> [--color always|never|auto]\n  \
                 ont project  <term>… [--reasoner] [--baseline] [--ttl] [--color <when>]\n  \
                 ont wordnet  <english-wordnet.ttl> [--out data/wordnet-pos.tsv.gz]"
            );
            std::process::exit(2);
        }
    }
}

/// The build discards roughly a third of every source; this shows which third, for one
/// term, without diffing two whole ontologies.
fn diff(args: &[String]) -> Result<()> {
    let (Some(source), Some(term)) = (args.get(2), args.get(3)) else {
        bail!("usage: ont diff <source> <term>\n  e.g. ont diff kbpedia Doctor-Medical");
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| "dist".into()));
    let cache = PathBuf::from(arg(args, "--cache").unwrap_or_else(|| "target/cache".into()));
    let recipes_dir = PathBuf::from(arg(args, "--recipes").unwrap_or_else(|| "recipes".into()));
    let recipe = Recipe::find(&recipes_dir, source)?;
    term::set_override(arg(args, "--color").as_deref());
    print!("{}", term::diff(&frona_ontologies::diff::run(&recipe, &cache, &out, term)?));
    Ok(())
}

/// The artifacts are only usable if a projection is cheap where the whole thing is not;
/// this keeps that a measurement rather than a claim.
fn project(args: &[String]) -> Result<()> {
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| "dist".into()));
    let terms: Vec<String> =
        args[2..].iter().take_while(|a| !a.starts_with("--")).cloned().collect();
    if terms.is_empty() {
        bail!(
            "usage: ont project <term>… [--reasoner] [--ttl]\n  e.g. ont project Doctor-Medical --reasoner"
        );
    }
    let opts = frona_ontologies::project::Options {
        reasoner: args.iter().any(|a| a == "--reasoner"),
        ttl: args.iter().any(|a| a == "--ttl"),
        baseline: args.iter().any(|a| a == "--baseline"),
    };
    term::set_override(arg(args, "--color").as_deref());
    print!("{}", term::report(&frona_ontologies::project::run(&out, &terms, &opts)?));
    Ok(())
}

/// One source parsed and filtered, held until every source is loaded so the catalogue
/// can be vetted whole. Distinct from `Recipe.build`, which is the *configuration* for
/// producing it.
struct Loaded<'a> {
    recipe: &'a Recipe,
    graph: Graph,
}

/// Reports and fails; it does not repair. An earlier version tried to find the responsible
/// alignment and drop it, and the attribution was a heuristic — it began rejecting
/// `foaf:Organization ≡ kbpedia:Organization`, which is plainly correct.
fn vet_catalogue(built: &[Loaded<'_>]) -> Result<Vec<String>> {
    let mut cat = Graph::default();
    for b in built.iter() {
        let (ttl, _) = emit::turtle(&b.graph, b.recipe);
        cat.absorb_bytes(ttl.as_bytes(), oxigraph::io::RdfFormat::Turtle)?;
    }
    cat.decompose_disjointness();
    let (eq, dj) = (cat.equivalence_index(), cat.disjointness_index());
    let mut unsat = Vec::new();
    for id in cat.declared() {
        if let Some((p, q)) = cat.union_clashes(id, id, &eq, &dj) {
            unsat.push(format!(
                "{} ⊑ both {} and {}",
                cat.term_name(id),
                cat.term_name(p),
                cat.term_name(q)
            ));
        }
    }
    Ok(unsat)
}

fn build(args: &[String]) -> Result<()> {
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| "dist".into()));
    let cache = PathBuf::from(arg(args, "--cache").unwrap_or_else(|| "target/cache".into()));
    let recipes_dir = PathBuf::from(arg(args, "--recipes").unwrap_or_else(|| "recipes".into()));
    let only = arg(args, "--source");

    let recipes = match &only {
        Some(n) => vec![Recipe::find(&recipes_dir, n)?],
        None => Recipe::all(&recipes_dir)?,
    };
    if recipes.is_empty() {
        bail!("no recipes found in {}", recipes_dir.display());
    }

    let mut pos = None;
    let mut credits: Vec<(String, String, String)> = Vec::new();

    let mut sources = Vec::new();
    let mut built: Vec<Loaded<'_>> = Vec::new();
    for r in &recipes {
        println!("\n▸ {} {}", r.name, r.version);
        let files = r.fetch(&cache)?;

        let mut g = Graph::default();
        let mut triples = 0usize;
        for f in &files {
            let n = g.absorb_file(f)?;
            triples += n;
            println!("    parse   {:<34}{n:>9} triples", f.file_name().unwrap().to_string_lossy());
        }
        g.decompose_disjointness();
        println!("    decompose disjointness → {} named pairs", g.disjoint.len());

        // Alignments are applied after the loop — they can only be vetted once every
        // source is loaded. Per source, the check reported nothing while thousands of
        // terms were unsatisfiable.

        if r.build.wordnet_noun_filter {
            if pos.is_none() {
                let w = r.wordnet.as_ref().with_context(|| {
                    format!("{}: wordnet_noun_filter needs a [wordnet] source", r.name)
                })?;
                let src = r.fetch_wordnet(&cache)?.expect("wordnet source declared");
                // Derived into the cache beside its source: 18 MB fetched once, 0.4 s to
                // reduce, and byte-identical every run.
                let index = cache.join("wordnet-pos.tsv.gz");
                if !index.exists() {
                    let n = build_pos_index(&src, &index)?;
                    println!("    wordnet            {n} lemmas → {}", index.display());
                }
                credits.push((
                    "Open English WordNet".into(),
                    w.license.clone(),
                    w.attribution.clone(),
                ));
                pos = Some(read_pos_index(&index)?);
            }
            let index = pos.as_ref().unwrap();
            let before = g.classes() + g.properties();

            // Two sources of seeds on equal footing: the lexical test judges each name
            // alone and cannot see that a term is load-bearing; axiom participation does.
            let mut seeds: HashSet<_> =
                g.declared().filter(|&id| is_thing_name(&g.term_name(id), index)).collect();
            let lexical = seeds.len();
            seeds.extend(g.axiom_participants());
            let from_axioms = seeds.len() - lexical;

            let keep = g.closure(&seeds);
            let pulled_back = keep.len() - seeds.len();
            g.restrict_to(keep);
            println!(
                "    seeds  {before} → {lexical} lexical +{from_axioms} axiom-bearing \
                 → closure +{pulled_back} → {} terms",
                g.classes() + g.properties()
            );
        }

        // These exist because the pipeline fails *silently* — a broken step emits a
        // plausible file with quietly missing content.
        if let Some(expected) = r.build.expect_disjoint_pairs
            && g.disjoint.len() != expected
        {
            bail!(
                "{}: expected {expected} disjointness pairs, produced {}. Either the \
                 recipe changed deliberately (update expect_disjoint_pairs) or the \
                 unionOf decomposition regressed.",
                r.name,
                g.disjoint.len()
            );
        }
        if let Some(min) = r.build.expect_min_probe_hits {
            let hits = probe::coverage(&g);
            if hits < min {
                bail!(
                    "{}: coverage probe resolved {hits}/{} everyday concepts, below the \
                     floor of {min} — the filter is over-pruning.",
                    r.name,
                    probe::PROBE.len()
                );
            }
            println!("    coverage probe     {hits}/{}", probe::PROBE.len());
        }

        built.push(Loaded { recipe: r, graph: g });
        let _ = triples;
    }

    // Imported here rather than per source: a contradiction between two vocabularies is
    // invisible while either is built alone. Refusing at the point of addition keeps the
    // consistent subset instead of forcing all-or-nothing.
    {
        let mut cat = Graph::default();
        for b in &built {
            let (ttl, _) = emit::turtle(&b.graph, b.recipe);
            cat.absorb_bytes(ttl.as_bytes(), oxigraph::io::RdfFormat::Turtle)?;
        }
        cat.decompose_disjointness();

        let mut accepted = Vec::new();
        let (mut n_ok, mut n_no) = (0usize, 0usize);
        for b in &built {
            for f in b.recipe.fetch_linkages(&cache)? {
                let bytes = std::fs::read(&f)?;
                let v = frona_ontologies::linkage::vet(&mut cat, &bytes)?;
                n_ok += v.accepted.len();
                for why in v.refused.iter().take(3usize.saturating_sub(n_no)) {
                    println!("    refused  {why}");
                }
                n_no += v.refused.len();
                accepted.extend(v.accepted);
            }
        }
        if n_ok + n_no > 0 {
            let applied: usize =
                built.iter_mut().map(|b| linkage::apply(&mut b.graph, &accepted)).sum();
            println!(
                "\n▸ alignments  {n_ok} accepted, {n_no} refused as contradictory \
                 ({applied} applied across sources)"
            );
        }
    }

    // Nothing may ship that makes a term unsatisfiable. This is not a nicety: the gate is
    // built on these axioms, so an unsatisfiable term is not an error the system catches —
    // it is the gate rejecting legitimate data forever.
    let unsat = vet_catalogue(&built)?;
    if !unsat.is_empty() {
        bail!(
            "catalogue: {} term(s) subsumed by two disjoint classes. They are unsatisfiable, \
             and the gate would fire on every page typed with one:\n  {}",
            unsat.len(),
            unsat.iter().take(10).cloned().collect::<Vec<_>>().join("\n  ")
        );
    }
    println!(
        "\n▸ catalogue  {} terms, none unsatisfiable",
        built.iter().map(|b| b.graph.declared().count()).sum::<usize>()
    );

    for Loaded { recipe: r, graph: g } in &built {
        println!("\n▸ {} {}", r.name, r.version);
        let (ttl, n) = emit::turtle(g, r);
        let artifact = emit::write_ttl(&out, &r.name, &ttl, n)?;
        println!(
            "    emit    {:<34}{:>9} triples  {:>6} KB gz  ({} KB raw)",
            artifact.name,
            artifact.triples,
            artifact.bytes / 1024,
            artifact.bytes_uncompressed / 1024
        );
        sources.push(emit::SourceMeta {
            name: r.name.clone(),
            version: r.version.clone(),
            upstream: r.upstream.clone(),
            license: r.license.clone(),
            attribution: r.attribution.clone(),
            classes: g.classes(),
            properties: g.properties(),
            disjoint_pairs: g.disjoint.len(),
            artifact,
        });
    }

    std::fs::create_dir_all(&out)?;
    std::fs::write(out.join("NOTICE"), emit::notice(&sources, &credits))?;
    emit::write_metadata(
        &out,
        &emit::Metadata {
            schema_version: 1,
            // No wall-clock: a rebuild of the same inputs must produce byte-identical
            // artifacts, so the release step can tell "nothing changed" from "changed".
            generated_at: std::env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| "unset".into()),
            sources,
        },
    )?;
    println!("\n✓ {}/", out.display());
    Ok(())
}

/// Derive the vendored part-of-speech index from a full WordNet distribution.
///
/// Open English WordNet is 202 MB; the index it reduces to is ~2 MB. Vendoring that
/// keeps CI from downloading a fifth of a gigabyte to answer "is this word a noun".
fn wordnet(args: &[String]) -> Result<()> {
    let src =
        args.get(2).context("usage: ont wordnet <english-wordnet.ttl[.gz]> [--out <path>]")?;
    let out = PathBuf::from(
        arg(args, "--out").unwrap_or_else(|| "target/cache/wordnet-pos.tsv.gz".into()),
    );
    let n = build_pos_index(Path::new(src), &out)?;
    println!("{n} lemmas → {}", out.display());
    Ok(())
}

/// Reduce Open English WordNet to `lemma\tPOS`. 212 MB of Turtle becomes 662 KB, and the
/// result is byte-identical across runs, so the build derives it into the cache rather than
/// carrying a committed copy of someone else's corpus.
fn build_pos_index(src: &Path, out: &Path) -> Result<usize> {
    let raw = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    let text = if src.extension().and_then(|e| e.to_str()) == Some("gz") {
        let mut v = String::new();
        flate2::read::GzDecoder::new(&raw[..])
            .read_to_string(&mut v)
            .with_context(|| format!("gunzip {}", src.display()))?;
        v
    } else {
        String::from_utf8(raw).context("wordnet source is not UTF-8")?
    };

    // Lemma IRIs encode part of speech in their fragment: …/lemma/office#office-n
    let mut by_lemma: std::collections::BTreeMap<String, HashSet<char>> = Default::default();
    let mut i = 0usize;
    while let Some(p) = text[i..].find("en-word.net/lemma/") {
        let s = i + p + "en-word.net/lemma/".len();
        let Some(hash) = text[s..].find('#') else { break };
        let lemma = &text[s..s + hash];
        let rest = &text[s + hash..];
        let end = rest.find(['>', ' ', '\n']).unwrap_or(rest.len());
        if let Some(pos) = rest[..end].rsplit('-').next()
            && pos.len() == 1
            && let Some(c) = pos.chars().next()
            && matches!(c, 'n' | 'v' | 'a' | 'r' | 's')
        {
            let key = lemma.replace('_', " ").to_lowercase();
            by_lemma.entry(key).or_default().insert(c);
        }
        i = s + hash;
    }
    if by_lemma.is_empty() {
        bail!("no lemmas found in {} — is it the Open English WordNet TTL?", src.display());
    }

    let mut body = String::new();
    for (lemma, pos) in &by_lemma {
        let mut p: Vec<char> = pos.iter().copied().collect();
        p.sort_unstable();
        body.push_str(lemma);
        body.push('\t');
        body.extend(p);
        body.push('\n');
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    emit::write_gz_bytes(out, &body)?;
    Ok(by_lemma.len())
}
