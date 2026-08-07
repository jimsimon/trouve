//! Default trouve desktop entry point (ADR 0027).

#[path = "web_preview.rs"]
mod web_preview;

fn main() -> anyhow::Result<()> {
    web_preview::run(true)
}
