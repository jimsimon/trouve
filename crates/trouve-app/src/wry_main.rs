//! Trouve desktop product entry point (ADR 0028).

#[path = "web_preview.rs"]
mod web_preview;

fn version_requested() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "--version" || argument == "-V")
}

fn main() -> anyhow::Result<()> {
    if web_preview::run_update_relaunch_supervisor()? {
        return Ok(());
    }
    let update_ready_acknowledgement = web_preview::take_update_ready_acknowledgement()?;
    web_preview::wait_for_update_relaunch_gate()?;
    if version_requested() {
        println!("trouve {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    web_preview::run(true, update_ready_acknowledgement)
}
