fn main() {
    // The widget crates' .slint sources are imported by path; within the
    // workspace they are stable siblings.
    let config = slint_build::CompilerConfiguration::new()
        .with_include_paths(vec![
            "../slint-code-view/ui".into(),
            "../slint-diff-view/ui".into(),
            "../slint-markdown/ui".into(),
            "../slint-terminal/ui".into(),
        ])
        // Geometry assertions use Slint's element handles in debug/test
        // builds. Release builds do not carry the extra element metadata.
        .with_debug_info(cfg!(debug_assertions));
    slint_build::compile_with_config("ui/app.slint", config).expect("slint compiles");
}
