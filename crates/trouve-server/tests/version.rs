use std::process::Command;

#[test]
fn server_binary_reports_workspace_version_without_starting_the_server() {
    for arguments in [
        vec!["--version"],
        vec!["-V"],
        vec!["--addr", "127.0.0.1:0", "--version"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_trouve-server"))
            .args(arguments)
            .output()
            .expect("run trouve-server version command");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            concat!("trouve-server ", env!("CARGO_PKG_VERSION"), "\n")
        );
        assert!(output.stderr.is_empty());
    }
}
