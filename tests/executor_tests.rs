use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use fast_context_rust::config::{RipgrepError, check_ripgrep_path};
use fast_context_rust::executor::ToolExecutor;
use serde_json::json;
use tempfile::TempDir;

fn make_project() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp project");
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::create_dir_all(root.join("vendor")).expect("vendor dir");
    fs::write(root.join("a.txt"), "alpha\nneedle in text\n").expect("a.txt");
    fs::write(root.join("b.txt"), "bravo\n").expect("b.txt");
    fs::write(root.join(".hidden"), "secret\n").expect("hidden");
    fs::write(
        root.join("src").join("app.js"),
        "export const needle = 'app';\n",
    )
    .expect("app.js");
    fs::write(root.join("src").join("ignored.test.js"), "needle in test\n")
        .expect("ignored.test.js");
    fs::write(root.join("vendor").join("lib.js"), "needle in vendor\n").expect("lib.js");
    (dir, root)
}

fn executor(root: &Path) -> ToolExecutor {
    ToolExecutor::with_rg_path(root, "rg").expect("executor")
}

#[test]
fn path_boundary_rejects_traversal_absolute_and_sibling_prefix() {
    let (_dir, root) = make_project();
    let executor = executor(&root);

    assert!(
        executor
            .readfile("/codebase/../../../etc/passwd", None, None)
            .contains("must stay within /codebase")
    );
    assert!(
        executor
            .readfile("/etc/passwd", None, None)
            .contains("must stay within /codebase")
    );
    assert!(
        executor
            .readfile("/codebaseevil/a.txt", None, None)
            .contains("must stay within /codebase")
    );
}

#[test]
fn readfile_formats_one_indexed_line_ranges_and_truncates() {
    let (_dir, root) = make_project();
    let executor = executor(&root);

    assert_eq!(
        executor.readfile("/codebase/a.txt", Some(1), Some(2)),
        "1:alpha\n2:needle in text"
    );
    assert_eq!(
        executor.readfile("/codebase/missing.txt", None, None),
        "Error: file not found: /codebase/missing.txt"
    );
}

#[test]
fn ls_tree_and_glob_remap_to_codebase() {
    let (_dir, root) = make_project();
    let executor = executor(&root);

    let ls = executor.ls("/codebase", false, false);
    assert!(ls.contains("a.txt"));
    assert!(ls.contains("src"));
    assert!(!ls.contains(".hidden"));

    let tree = executor.tree("/codebase", Some(1));
    assert!(tree.starts_with("/codebase"));
    assert!(tree.contains("a.txt"));
    assert!(!tree.contains(root.to_string_lossy().as_ref()));

    let glob = executor.glob("**/*.js", "/codebase", "file");
    assert!(glob.contains("/codebase/src/app.js"));
    assert!(glob.contains("/codebase/vendor/lib.js"));
    assert!(!glob.contains(root.to_string_lossy().as_ref()));
}

#[test]
fn rg_returns_no_match_sentinel() {
    let (_dir, root) = make_project();
    let executor = executor(&root);

    assert_eq!(
        executor.rg("this-pattern-does-not-exist", "/codebase", None, None),
        "(no matches)"
    );
}

#[test]
fn rg_honors_include_exclude_and_remaps_output() {
    let (_dir, root) = make_project();
    let executor = executor(&root);
    let include = vec!["**/*.js".to_string(), "*.js".to_string()];
    let exclude = vec!["**/*.test.js".to_string(), "vendor/**".to_string()];

    let out = executor.rg("needle", "/codebase", Some(&include), Some(&exclude));
    assert!(out.contains("/codebase/src/app.js:1:"));
    assert!(!out.contains("ignored.test.js"));
    assert!(!out.contains("vendor/lib.js"));
    assert!(!out.contains(root.to_string_lossy().as_ref()));
}

#[test]
fn rg_missing_executable_is_user_visible_and_checkable() {
    let (_dir, root) = make_project();
    let executor = ToolExecutor::with_rg_path(&root, "definitely-missing-rg-for-fast-context-test")
        .expect("executor");
    let out = executor.rg("needle", "/codebase", None, None);
    assert!(out.contains("Error:"));
    assert!(
        out.to_lowercase().contains("no such file") || out.to_lowercase().contains("not found")
    );

    let missing = check_ripgrep_path(Path::new("definitely-missing-rg-for-fast-context-test"))
        .expect_err("missing rg should error");
    match missing {
        RipgrepError::NotFound { .. } => {
            assert!(missing.user_message().contains("Install examples"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn exec_tool_call_sorts_command_keys_numerically() {
    let (_dir, root) = make_project();
    let executor = executor(&root);

    let out = executor.exec_tool_call(&json!({
        "command10": { "type": "readfile", "file": "/codebase/b.txt" },
        "command2": { "type": "readfile", "file": "/codebase/a.txt" },
        "command1": { "type": "readfile", "file": "/codebase/a.txt" }
    }));

    assert!(out.find("<command1_result>").unwrap() < out.find("<command2_result>").unwrap());
    assert!(out.find("<command2_result>").unwrap() < out.find("<command10_result>").unwrap());
}

#[test]
fn truncate_matches_node_core_invariants() {
    let dir = tempfile::tempdir().expect("temp project");
    let mut file = File::create(dir.path().join("long.txt")).expect("long file");
    for idx in 0..55 {
        writeln!(file, "line-{idx:02}-{}", "x".repeat(300)).expect("write line");
    }
    let executor = executor(dir.path());
    let out = executor.readfile("/codebase/long.txt", None, None);
    assert!(out.contains("... (lines truncated) ..."));
    assert!(out.lines().next().unwrap().len() <= "1:".len() + 250);
}
