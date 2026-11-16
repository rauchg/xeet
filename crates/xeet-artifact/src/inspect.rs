//! A human-readable account of an artifact: what it is, where it came from,
//! what it says, what it lets you do, and what it does not know.

use crate::model::*;
use std::fmt::Write;

pub fn text(a: &Artifact) -> String {
    let mut s = String::new();
    let p = &a.provenance;
    let _ = writeln!(s, "{} {}", a.subject.kind, a.subject.title);
    let _ = writeln!(s, "  id           {}", a.id);
    if let Some(r) = &a.subject.reference {
        let _ = writeln!(s, "  reference    {r}");
    }
    let _ = writeln!(
        s,
        "  observed     {} via {} ({} {})",
        p.capture.captured_at,
        p.capture.method,
        p.capture.captured_by.name,
        p.capture.captured_by.version
    );
    let _ = writeln!(s, "  adapter      {} {}", p.adapter.name, p.adapter.version);
    let _ = writeln!(s, "  authenticity {:?}", p.authenticity);
    let _ = writeln!(
        s,
        "  NOTE         a historical observation as of {}; nothing here is current state",
        a.state.as_of
    );

    let _ = writeln!(s, "\nviews");
    for v in &a.views {
        let _ = writeln!(
            s,
            "  {}  {} {}x{} px (scale {}) by {} {}{}  fidelity: {:?} — {}",
            v.id,
            v.media_type,
            v.pixel_width,
            v.pixel_height,
            v.scale,
            v.renderer.name,
            v.renderer.version,
            v.renderer
                .ruleset
                .as_ref()
                .map(|r| format!(" [{r}]"))
                .unwrap_or_default(),
            v.fidelity.claim,
            v.fidelity.target
        );
    }

    let _ = writeln!(
        s,
        "\nsemantic ({} elements, {} relations)",
        a.semantic.elements.len(),
        a.semantic.relations.len()
    );
    tree(a, &a.semantic.root, 1, &mut s);
    for r in &a.semantic.relations {
        let _ = writeln!(s, "  {} —{}→ {}", r.from, r.kind, r.to);
    }

    let _ = writeln!(s, "\nstate as of {}", a.state.as_of);
    for f in &a.state.facts {
        let _ = writeln!(
            s,
            "  {}.{} = {}  [{:?}{}]",
            f.subject,
            f.key,
            f.value,
            f.basis,
            if f.volatile { ", may have changed" } else { "" }
        );
    }

    let _ = writeln!(
        s,
        "\ncapabilities ({}; descriptions, not grants)",
        a.capability_policy
    );
    for c in &a.capabilities {
        let auth = if c.authority.required.is_empty() {
            "no authority".to_string()
        } else {
            format!(
                "needs {} (checked by {})",
                c.authority.required.join(", "),
                c.authority.checked_by
            )
        };
        let _ = writeln!(
            s,
            "  {:<28} {:?}/{:?} → {:?} {}; {auth}",
            c.id, c.kind, c.availability, c.executor.kind, c.executor.reference
        );
    }

    let _ = writeln!(s, "\nlimitations");
    for (name, notes) in [
        ("omitted", &a.limitations.omitted),
        ("approximated", &a.limitations.approximated),
        ("derived", &a.limitations.derived),
        ("inferred", &a.limitations.inferred),
        ("unverified", &a.limitations.unverified),
    ] {
        for n in notes.iter() {
            let subject = n
                .subject
                .as_deref()
                .map(|x| format!("[{x}] "))
                .unwrap_or_default();
            let _ = writeln!(s, "  {name:<12} {subject}{} — {}", n.what, n.why);
        }
    }
    let _ = writeln!(s, "\ncoverage");
    for (k, v) in &a.coverage {
        let _ = writeln!(s, "  {k:<13} {v:?}");
    }
    s
}

fn tree(a: &Artifact, id: &str, depth: usize, s: &mut String) {
    if depth > 32 {
        return;
    }
    let Some(e) = a.semantic.elements.iter().find(|e| e.id == id) else {
        return;
    };
    let mut line = format!("{}{} {} ({})", "  ".repeat(depth), e.id, e.kind, e.role);
    if let Some(t) = &e.text {
        let short: String = t.chars().take(60).collect();
        let more = if t.chars().count() > 60 { "…" } else { "" };
        let _ = write!(line, " {:?}", format!("{short}{more}"));
    }
    if let Some(v) = &e.value {
        let _ = write!(line, " = {v}");
        if let Some(u) = &e.unit {
            let _ = write!(line, " {u}");
        }
    }
    if !e.visible {
        line.push_str(" [not visible]");
    }
    if let Some(b) = e.bounds.first() {
        let _ = write!(
            line,
            " @{}({:.0},{:.0} {:.0}x{:.0})",
            b.view, b.x, b.y, b.w, b.h
        );
    }
    let _ = writeln!(s, "{line}");
    for c in &e.children {
        tree(a, c, depth + 1, s);
    }
}
