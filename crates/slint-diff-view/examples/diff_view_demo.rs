//! Spike demo: a ~3k-line synthetic diff with per-file collapse.
//! Run with: cargo run -p slint-diff-view --example diff_view_demo

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint_diff_view::{parse_unified_diff, rows_model, DiffViewWindow};

fn main() {
    let mut diff = String::new();
    for f in 0..30 {
        diff.push_str(&format!("diff --git a/src/file_{f}.rs b/src/file_{f}.rs\n"));
        diff.push_str(&format!("@@ -1,60 +1,70 @@ fn module_{f}()\n"));
        for l in 0..50 {
            diff.push_str(&format!(" context line {l} of file {f}\n"));
            if l % 5 == 0 {
                diff.push_str(&format!("-    old_value_{l} = {l};\n"));
                diff.push_str(&format!("+    new_value_{l} = {};\n", l * 2));
            }
        }
    }

    let files = Rc::new(parse_unified_diff(&diff));
    let collapsed = Rc::new(RefCell::new(vec![false; files.len()]));

    let window = DiffViewWindow::new().unwrap();
    window.set_rows(rows_model(&files, &collapsed.borrow()));

    let weak = window.as_weak();
    window.on_file_toggled(move |index| {
        let mut state = collapsed.borrow_mut();
        if let Some(flag) = state.get_mut(index as usize) {
            *flag = !*flag;
        }
        weak.unwrap().set_rows(rows_model(&files, &state));
    });

    window.run().unwrap();
}
