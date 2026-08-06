use std::process::Command;

#[test]
fn search_binary_reports_workspace_version() {
    for argument in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_trouve-search"))
            .arg(argument)
            .output()
            .expect("run trouve-search version command");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            concat!("trouve-search ", env!("CARGO_PKG_VERSION"), "\n")
        );
        assert!(output.stderr.is_empty());
    }
}
