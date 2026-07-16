//! Spike demo: streams a markdown document into the view at ~50 deltas/s,
//! the way assistant output arrives. Run with:
//! cargo run -p slint-markdown --example markdown_demo

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint_markdown::{MarkdownWindow, StreamingMarkdown};

const DOCUMENT: &str = r#"# Streaming markdown

This paragraph arrives token by token, the same way assistant output
streams from the model. The renderer only updates trailing blocks.

## What to watch

- the paragraph above growing without flicker
- bullets appearing one at a time
- a code fence that renders while still open

```rust
fn main() {
    println!("hello from the fence");
}
```

### Done

Steady state reached — the whole document was streamed.
"#;

fn main() {
    let window = MarkdownWindow::new().unwrap();
    let streaming = Rc::new(RefCell::new(StreamingMarkdown::new()));
    window.set_blocks(streaming.borrow().model());

    let chars: Vec<char> = DOCUMENT.chars().collect();
    let position = Rc::new(RefCell::new(0usize));
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(20),
        move || {
            let mut pos = position.borrow_mut();
            if *pos >= chars.len() {
                return;
            }
            let end = (*pos + 5).min(chars.len());
            let delta: String = chars[*pos..end].iter().collect();
            *pos = end;
            streaming.borrow_mut().push(&delta);
        },
    );

    window.run().unwrap();
}
