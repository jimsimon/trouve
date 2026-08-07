//! Trouve desktop product entry point (ADR 0028).

#[path = "web_preview.rs"]
mod web_preview;

fn main() -> anyhow::Result<()> {
    web_preview::run(true)
}
