fn main() {
    // The widget crates' .slint sources are imported by path; within the
    // workspace they are stable siblings.
    let config = slint_build::CompilerConfiguration::new().with_include_paths(vec![
        "../trouve-slint-code-view/ui".into(),
        "../trouve-slint-diff-view/ui".into(),
        "../trouve-slint-markdown/ui".into(),
        "../trouve-slint-terminal/ui".into(),
    ]);
    slint_build::compile_with_config("ui/app.slint", config).expect("slint compiles");
}
