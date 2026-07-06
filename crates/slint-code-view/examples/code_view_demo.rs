//! Spike demo: a 10,000-line file with fake highlight spans in the
//! virtualized code view. Run with: cargo run -p slint-code-view --example code_view_demo

use slint::ComponentHandle;
use slint_code_view::{copy_text_from, line_numbers_model, lines_model, CodeViewWindow, Span};

fn main() {
    // Generate a big file with a few colored spans per line.
    let mut text = String::new();
    let mut spans: Vec<Vec<Span>> = Vec::new();
    for i in 0..10_000 {
        text.push_str(&format!(
            "fn generated_{i}(value: usize) -> usize {{ value * {i} }} // line {i}\n"
        ));
        spans.push(vec![
            Span {
                start: 0,
                end: 2,
                color: 0x569cd6,
            },
            Span {
                start: 3,
                end: 13 + i.to_string().len(),
                color: 0xdcdcaa,
            },
            Span {
                start: 24,
                end: 29,
                color: 0x4ec9b0,
            },
        ]);
    }

    let view = CodeViewWindow::new().unwrap();
    view.set_lines(lines_model(&text, &spans));
    view.set_line_numbers(line_numbers_model(&text));

    let weak = view.as_weak();
    let source = text.clone();
    view.on_copy_requested(move || {
        let view = weak.unwrap();
        if let Some(selected) = copy_text_from(&view, &source) {
            // A real app hands this to the clipboard (e.g. via arboard);
            // the spike verifies extraction correctness.
            println!(
                "copied {} bytes: {:?}",
                selected.len(),
                &selected[..selected.len().min(120)]
            );
        }
    });

    view.run().unwrap();
}
