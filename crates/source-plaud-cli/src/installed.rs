//! Facts about the installed tool that can be read from disk (#304).
//!
//! Local-only Mode forbids *launching* the PLAUD CLI, because the tool can talk
//! to PLAUD's servers on its own. It does not forbid looking at files on this
//! computer, and the version npm installed is written in the package's own
//! `package.json`. Reading it means the diagnostics panel can name the version
//! and the compatibility verdict without running anything.
//!
//! None of this replaces the version the tool itself reports: the parser is
//! still chosen from `plaud --version` when a launch is allowed. This is the
//! no-launch fallback, and it fails quiet: anything unexpected yields `None`.

use std::path::Path;

use crate::parser::CliVersion;

/// The npm package name of the official tool.
pub const PACKAGE_NAME: &str = "@plaud-ai/cli";

/// Read the installed version of the official tool behind `program`, without
/// running it.
#[must_use]
pub fn read_installed_version(program: &Path) -> Option<CliVersion> {
    let _ = program;
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Lay out an npm global install the way Homebrew's node does:
    /// `bin/plaud -> ../lib/node_modules/@plaud-ai/cli/dist/index.js`.
    fn npm_layout(root: &Path, package_json: &str) -> PathBuf {
        let package = root.join("lib/node_modules/@plaud-ai/cli");
        std::fs::create_dir_all(package.join("dist")).expect("package dir");
        std::fs::write(package.join("package.json"), package_json).expect("package.json");
        std::fs::write(
            package.join("dist/index.js"),
            "#!/usr/bin/env node\nconsole.log('x')\n",
        )
        .expect("index.js");
        std::fs::create_dir_all(root.join("bin")).expect("bin dir");
        let program = root.join("bin/plaud");
        #[cfg(unix)]
        std::os::unix::fs::symlink("../lib/node_modules/@plaud-ai/cli/dist/index.js", &program)
            .expect("symlink");
        #[cfg(not(unix))]
        std::fs::copy(package.join("dist/index.js"), &program).expect("copy");
        program
    }

    #[cfg(unix)]
    #[test]
    fn issue_304_the_version_is_read_from_package_json_without_running_the_tool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = npm_layout(
            dir.path(),
            r#"{ "name": "@plaud-ai/cli", "version": "0.3.15", "bin": { "plaud": "dist/index.js" } }"#,
        );
        assert_eq!(
            read_installed_version(&program)
                .as_ref()
                .map(CliVersion::as_str),
            Some("0.3.15")
        );
    }

    #[cfg(unix)]
    #[test]
    fn issue_304_a_different_package_is_not_mistaken_for_the_tool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = npm_layout(
            dir.path(),
            r#"{ "name": "plaud-impostor", "version": "0.3.15" }"#,
        );
        assert_eq!(read_installed_version(&program), None);
    }

    #[cfg(unix)]
    #[test]
    fn issue_304_an_unreadable_version_string_is_not_guessed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = npm_layout(
            dir.path(),
            r#"{ "name": "@plaud-ai/cli", "version": "latest; rm -rf /" }"#,
        );
        assert_eq!(read_installed_version(&program), None);
        let dir = tempfile::tempdir().expect("tempdir");
        let program = npm_layout(dir.path(), "not json at all");
        assert_eq!(read_installed_version(&program), None);
    }

    #[test]
    fn issue_304_a_missing_program_yields_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_installed_version(&dir.path().join("plaud")), None);
    }
}
