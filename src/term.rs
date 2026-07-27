//! Colouring for terminal output.
//!
//! Applied as a post-pass over finished text rather than woven into the writers, so what
//! gets coloured is exactly what gets piped, redirected or diffed.
//!
//! Detection is `owo-colors`' job, not ours: `NO_COLOR`, `CLICOLOR_FORCE`, dumb terminals
//! and Windows consoles are a longer matrix than an `IsTerminal` check, and getting it
//! wrong means escape codes in someone's redirected file.

use owo_colors::{OwoColorize, Stream::Stdout};

/// `--color <always|never|auto>`. `auto` defers to the terminal; the other two are the
/// escape hatch for pipelines that do want codes, or terminals that shouldn't get them.
pub fn set_override(when: Option<&str>) {
    match when {
        Some("always") => owo_colors::set_override(true),
        Some("never") => owo_colors::set_override(false),
        _ => owo_colors::unset_override(),
    }
}

/// Markers sit in column 0, so the first character decides.
pub fn diff(text: &str) -> String {
    paint(text, |line| match line.chars().next() {
        Some('-') => line.if_supports_color(Stdout, |t| t.red()).to_string(),
        Some('+') => line.if_supports_color(Stdout, |t| t.green()).to_string(),
        Some('#') | Some('@') => line.if_supports_color(Stdout, |t| t.dimmed()).to_string(),
        // A subject sits in column 0 with no marker; its predicates are indented.
        Some(c) if !c.is_whitespace() => line.if_supports_color(Stdout, |t| t.bold()).to_string(),
        _ => line.to_string(),
    })
}

pub fn report(text: &str) -> String {
    paint(text, |line| {
        let t = line.trim_start();
        if t.starts_with("CLASH") || t.starts_with("MISSED") || t.starts_with("EXTRA") {
            line.if_supports_color(Stdout, |x| x.red()).to_string()
        } else if t.starts_with('#') {
            line.if_supports_color(Stdout, |x| x.dimmed()).to_string()
        } else if line.starts_with(char::is_alphabetic) {
            // Only the label that opens a section, so the numbers stay readable.
            match line.split_once(char::is_whitespace) {
                Some((label, rest)) => {
                    format!("{}{rest}", label.if_supports_color(Stdout, |x| x.bold()))
                }
                None => line.if_supports_color(Stdout, |x| x.bold()).to_string(),
            }
        } else {
            line.to_string()
        }
    })
}

fn paint(text: &str, style: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for line in text.lines() {
        out.push_str(&style(line));
        out.push('\n');
    }
    out
}
