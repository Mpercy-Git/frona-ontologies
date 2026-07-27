//! Recipes — the declarative description of a source and how it is built.
//!
//! One TOML file per source in `recipes/`. Versions are **pinned here**, so upgrading a
//! source is a one-line reviewable change rather than something that happens because
//! upstream published.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Recipe {
    pub name: String,
    pub version: String,
    pub upstream: String,
    pub license: String,
    pub attribution: String,
    #[serde(default)]
    pub files: Vec<SourceFile>,
    /// KBpedia's alignment tables, listed by every vocabulary an alignment touches; each
    /// absorbs the rows it declares the subject of.
    #[serde(default)]
    pub linkages: Vec<SourceFile>,
    /// Open English WordNet, for recipes whose seed test needs a part-of-speech lookup.
    /// Fetched and cached like everything else rather than committed, so its licence flows
    /// into `NOTICE` through the same path as an ontology's.
    pub wordnet: Option<WordNet>,
    #[serde(default)]
    pub build: Build,
}

#[derive(Debug, Deserialize)]
pub struct SourceFile {
    pub url: String,
    /// Also selects the parser, by extension.
    pub as_file: String,
    #[serde(default)]
    pub unzip: bool,
}

#[derive(Debug, Deserialize)]
pub struct WordNet {
    pub url: String,
    pub as_file: String,
    pub license: String,
    pub attribution: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Build {
    /// Off for small curated vocabularies like schema.org, where every term is wanted.
    #[serde(default)]
    pub wordnet_noun_filter: bool,
    /// UNUSED — no code reads this. Named for a separate labels artifact that was never
    /// built; the decision was to keep labels in the one file per source.
    #[serde(default = "yes")]
    pub emit_labels: bool,
    /// Assert the built disjointness count exactly. The pipeline's most important
    /// guard: a transposed `rdf:first`/`rdf:rest` silently took this 666 → 20 during
    /// prototyping, producing a plausible file and no error.
    #[serde(default)]
    pub expect_disjoint_pairs: Option<usize>,
    /// Everyday concepts that must survive the build, as a floor on how many resolve.
    /// Catches a filter that over-prunes.
    #[serde(default)]
    pub expect_min_probe_hits: Option<usize>,
}

fn yes() -> bool {
    true
}

impl Recipe {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read recipe {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse recipe {}", path.display()))
    }

    pub fn find(dir: &Path, name: &str) -> Result<Self> {
        let p = dir.join(format!("{name}.toml"));
        if !p.exists() {
            bail!("no recipe for '{name}' at {}", p.display());
        }
        Self::load(&p)
    }

    pub fn all(dir: &Path) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("read recipes dir {}", dir.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
            .collect();
        paths.sort();
        for p in paths {
            out.push(Self::load(&p)?);
        }
        Ok(out)
    }

    /// Sources are pinned and immutable, so a present file is never re-fetched.
    pub fn fetch(&self, cache: &Path) -> Result<Vec<PathBuf>> {
        self.fetch_list(cache, &self.files)
    }

    pub fn fetch_linkages(&self, cache: &Path) -> Result<Vec<PathBuf>> {
        self.fetch_list(cache, &self.linkages)
    }

    pub fn fetch_wordnet(&self, cache: &Path) -> Result<Option<PathBuf>> {
        let Some(w) = &self.wordnet else { return Ok(None) };
        let f = SourceFile { url: w.url.clone(), as_file: w.as_file.clone(), unzip: false };
        Ok(self.fetch_list(cache, std::slice::from_ref(&f))?.pop())
    }

    fn fetch_list(&self, cache: &Path, list: &[SourceFile]) -> Result<Vec<PathBuf>> {
        std::fs::create_dir_all(cache)?;
        let mut out = Vec::new();
        for f in list {
            let dest = cache.join(&f.as_file);
            if dest.exists() {
                println!("    cached  {}", f.as_file);
                out.push(dest);
                continue;
            }
            println!("    fetch   {} ← {}", f.as_file, f.url);
            let body = ureq::get(&f.url)
                .call()
                .with_context(|| format!("GET {}", f.url))?
                .body_mut()
                // ureq caps a response at 10 MB by default, which silently turned the
                // 18 MB WordNet download into "read body of …" with no mention of size.
                .with_config()
                .limit(256 * 1024 * 1024)
                .read_to_vec()
                .with_context(|| format!("read body of {}", f.url))?;
            if f.unzip {
                let mut zip =
                    zip_single_member(&body).with_context(|| format!("unzip {}", f.as_file))?;
                std::fs::write(&dest, &mut zip)?;
            } else {
                std::fs::write(&dest, &body)?;
            }
            out.push(dest);
        }
        Ok(out)
    }
}

/// Minimal single-member zip reader — enough for KBpedia's published archives, without
/// taking a zip crate for one call site. Handles stored and deflated entries.
fn zip_single_member(bytes: &[u8]) -> Result<Vec<u8>> {
    // Local file header: 0x04034b50, name len at 26, extra len at 28, data follows.
    if bytes.len() < 30 || &bytes[0..4] != b"PK\x03\x04" {
        bail!("not a zip archive");
    }
    let method = u16::from_le_bytes([bytes[8], bytes[9]]);
    let name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
    let extra_len = u16::from_le_bytes([bytes[28], bytes[29]]) as usize;
    let start = 30 + name_len + extra_len;
    let data = &bytes[start..];
    match method {
        0 => Ok(data.to_vec()),
        8 => {
            use std::io::Read;
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(data).read_to_end(&mut out)?;
            Ok(out)
        }
        m => bail!("unsupported zip compression method {m}"),
    }
}
