//! Regenerates `tests/fixtures/gateway/invoices.png`, the screenshot the
//! gateway's end-to-end tests use. Deterministic: xeet's document renderer.
//!
//!   cargo run -p xeet-gateway --example make_invoice_fixture

use xeet_capture::view::{self, Block, Entry, Tone, ViewOptions};

fn main() {
    let rows = [
        (
            "INV-1041",
            "Northwind Traders",
            "$1,200.00",
            "2026-10-01",
            "Overdue",
            Tone::Danger,
        ),
        (
            "INV-1042",
            "Contoso Ltd",
            "$480.00",
            "2026-11-30",
            "Open",
            Tone::Neutral,
        ),
        (
            "INV-1043",
            "Fabrikam Inc",
            "$2,950.00",
            "2026-09-15",
            "Overdue",
            Tone::Danger,
        ),
        (
            "INV-1044",
            "Tailspin Toys",
            "$130.00",
            "2026-11-01",
            "Paid",
            Tone::Success,
        ),
        (
            "INV-1045",
            "Wingtip Toys",
            "$760.00",
            "2026-10-20",
            "Overd…",
            Tone::Attention,
        ),
    ];
    let blocks = vec![
        Block::Meta {
            el: "app".into(),
            text: "Acme Billing · Receivables".into(),
        },
        Block::Heading {
            el: "title".into(),
            text: "Invoices".into(),
        },
        Block::Meta {
            el: "asof".into(),
            text: "As of 2026-11-16 · 5 invoices".into(),
        },
        Block::Section {
            el: "table".into(),
            title: "Invoice · Customer · Amount · Due · Status".into(),
            items: rows
                .iter()
                .map(|(id, c, amt, due, st, tone)| Entry {
                    el: id.to_string(),
                    primary: format!("{id} · {c} · {amt}"),
                    secondary: Some(format!("Due {due} · {st}")),
                    tone: *tone,
                })
                .collect(),
        },
    ];
    let out = view::render(
        &blocks,
        &ViewOptions {
            width: 480.0,
            scale: 1.0,
            ..ViewOptions::default()
        },
    );
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/gateway/invoices.png"
    );
    std::fs::write(path, &out.png).unwrap();
    println!(
        "{path}: {} bytes, {}x{}",
        out.png.len(),
        out.width,
        out.height
    );
    for (id, r) in &out.bounds {
        println!("  {id}: {} {} {} {}", r.x, r.y, r.w, r.h);
    }
}
