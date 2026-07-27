//! The coverage probe.
//!
//! A fixed list of everyday concepts a personal knowledge base actually records. It is
//! not a quality score — it is a **floor**, there to fail the build when a filter change
//! silently over-prunes. Measured against KBpedia 2.50 the built output resolves
//! 49 of these; the recipe asserts a minimum, so an over-eager filter shows up as a
//! failed build rather than a vocabulary that quietly lost `physician`.

use std::collections::HashSet;

use crate::graph::Graph;

pub const PROBE: &[&str] = &[
    // people and relationships
    "friend",
    "colleague",
    "physician",
    "dentist",
    "landlord",
    "neighbour",
    "manager",
    "client",
    "teacher",
    "nurse",
    // events
    "meeting",
    "appointment",
    "birthday",
    "trip",
    "flight",
    "conference",
    "interview",
    "wedding",
    "holiday",
    // places
    "restaurant",
    "cafe",
    "gym",
    "airport",
    "hotel",
    "apartment",
    "office",
    "hospital",
    "school",
    // work
    "project",
    "task",
    "deadline",
    "invoice",
    "contract",
    "salary",
    "employer",
    "job",
    // health
    "medication",
    "symptom",
    "allergy",
    "exercise",
    "diet",
    "prescription",
    "vaccination",
    "disease",
    // home and life
    "recipe",
    "subscription",
    "warranty",
    "insurance",
    "car",
    "bicycle",
    "pet",
    "houseplant",
    // media
    "book",
    "movie",
    "podcast",
    "article",
    "album",
    "video game",
    "newspaper",
    "blog",
    // finance
    "bank account",
    "credit card",
    "expense",
    "budget",
    "loan",
    "tax",
    "mortgage",
    // technical
    "database",
    "api",
    "software repository",
    "server",
    "programming language",
    "framework",
    "operating system",
    "web browser",
];

/// How many probe concepts the built graph can still name.
pub fn coverage(g: &Graph) -> usize {
    let names: HashSet<String> = g.declared().map(|id| g.term_name(id)).collect();
    PROBE.iter().filter(|p| names.contains(**p)).count()
}
