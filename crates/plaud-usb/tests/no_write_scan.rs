//! Spec 7.8: nothing in this crate may write to the device volume.
//!
//! Scans the non-test source of every module for write-capable filesystem
//! calls. Test code (everything from the first `#[cfg(test)]`) and comments
//! are excluded.

use std::path::Path;

const FORBIDDEN: &[&str] = &[
    "OpenOptions",
    ".write(true)",
    ".append(true)",
    ".truncate(true)",
    "File::create",
    "create_new",
    "create_dir",
    "remove_file",
    "remove_dir",
    "rename(",
    "set_permissions",
    "fs::write",
    "fs::copy",
    "hard_link",
    "fs::symlink(",
    "symlink_file(",
    "symlink_dir(",
    "set_len",
    "set_modified",
];

fn production_lines(source: &str) -> Vec<(usize, &str)> {
    let cut = source.find("#[cfg(test)]").unwrap_or(source.len());
    source[..cut]
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .map(|(i, line)| (i + 1, line))
        .collect()
}

#[test]
fn no_module_opens_creates_renames_removes_or_re_permissions_anything() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0;
    let mut violations = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        checked += 1;
        for (line_no, line) in production_lines(&source) {
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    violations.push(format!("{}:{line_no}: {needle}", path.display()));
                }
            }
        }
    }
    assert!(
        checked >= 5,
        "scanned only {checked} files; the scan is broken"
    );
    assert!(
        violations.is_empty(),
        "write-capable calls found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_scan_would_catch_a_write_call() {
    let sample = "fn f() {\n    std::fs::remove_file(p);\n}\n#[cfg(test)]\nmod t { fn g() { std::fs::remove_file(p); } }\n";
    let lines = production_lines(sample);
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().any(|(_, l)| l.contains("remove_file")));
}
