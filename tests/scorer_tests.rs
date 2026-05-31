use fast_context_rust::scorer::{hot_dirs, path_spines, quick_score, rank_paths, tokenize};

#[test]
fn tokenize_splits_camel_snake_and_paths() {
    let tokens = tokenize("src/FastContextEngine/read_file_parser.rs");

    assert!(tokens.contains(&"fast".to_string()));
    assert!(tokens.contains(&"context".to_string()));
    assert!(tokens.contains(&"engine".to_string()));
    assert!(tokens.contains(&"read".to_string()));
    assert!(tokens.contains(&"file".to_string()));
    assert!(tokens.contains(&"parser".to_string()));
    assert!(tokens.contains(&"rs".to_string()));
}

#[test]
fn quick_score_prefers_matching_basename_and_tokens() {
    let auth_score = quick_score("auth token", "src/auth/token_store.rs");
    let unrelated_score = quick_score("auth token", "docs/readme.md");

    assert!(auth_score > unrelated_score);
}

#[test]
fn rank_paths_is_stable_for_ties() {
    let paths = vec![
        "b/file.rs".to_string(),
        "a/file.rs".to_string(),
        "src/auth_token.rs".to_string(),
    ];
    let ranked = rank_paths("auth token", &paths);

    assert_eq!(ranked[0].path, "src/auth_token.rs");
    assert!(ranked[1].path < ranked[2].path);
}

#[test]
fn path_spines_and_hot_dirs_sort_by_count_then_path() {
    let paths = vec![
        "src/auth/login.rs".to_string(),
        "src/auth/token.rs".to_string(),
        "src/core/mod.rs".to_string(),
        "tests/auth/login_test.rs".to_string(),
    ];

    let spines = path_spines(&paths);
    assert_eq!(spines[0].path, "src");
    assert_eq!(spines[0].score, 3.0);

    let dirs = hot_dirs(&paths, 2);
    assert_eq!(dirs[0].path, "src/auth");
    assert_eq!(dirs[0].score, 2.0);
    assert_eq!(dirs.len(), 2);
}
