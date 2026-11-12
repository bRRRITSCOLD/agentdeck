use std::process::Command;

mod agentdeckd {
    use super::*;

    #[test]
    fn should_print_its_name_and_version_for_version_flag() {
        let output = Command::new(env!("CARGO_BIN_EXE_agentdeckd"))
            .arg("--version")
            .output()
            .expect("agentdeckd runs");

        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            concat!("agentdeckd ", env!("CARGO_PKG_VERSION"), "\n")
        );
    }
}
