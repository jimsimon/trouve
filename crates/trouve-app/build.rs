fn main() {
    // The widget crates' .slint sources are imported by path; within the
    // workspace they are stable siblings.
    let config = slint_build::CompilerConfiguration::new().with_include_paths(vec![
        "../slint-code-view/ui".into(),
        "../slint-diff-view/ui".into(),
        "../slint-markdown/ui".into(),
        "../slint-media-view/ui".into(),
        "../slint-terminal/ui".into(),
    ]);
    slint_build::compile_with_config("ui/app.slint", config).expect("slint compiles");
}
