//! Demo: streams colored fake build output into the scrollback.
//! Run with: cargo run -p trouve-slint-terminal --example terminal_demo

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use trouve_slint_terminal::{Scrollback, TerminalWindow};

fn main() {
    let window = TerminalWindow::new().unwrap();
    let scrollback = Rc::new(RefCell::new(Scrollback::new(5000)));
    window.set_lines(scrollback.borrow().model());

    let weak = window.as_weak();
    let counter = Rc::new(RefCell::new(0u32));
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(30),
        move || {
            let mut n = counter.borrow_mut();
            *n += 1;
            let line = match *n % 5 {
                0 => format!("\u{1b}[32m   Compiling\u{1b}[0m crate-{n} v0.{n}.0\n"),
                1 => format!("    checking module {n} ... \u{1b}[92mok\u{1b}[0m\n"),
                2 => format!("\u{1b}[33mwarning\u{1b}[0m: unused variable in file_{n}.rs\n"),
                3 => format!("\u{1b}[31merror\u{1b}[0m[E{n:04}]: recovered, continuing\n"),
                _ => format!("    plain progress line {n}\n"),
            };
            scrollback.borrow_mut().push(&line);
            weak.unwrap().invoke_scroll_to_end();
        },
    );

    window.run().unwrap();
}
