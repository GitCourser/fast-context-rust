use fast_context_rust::error::FastContextErrorKind;
use fast_context_rust::extract_key::{default_windsurf_db_path, extract_key_from_db_path};
use rusqlite::Connection;

#[test]
fn default_windsurf_db_path_points_at_windsurf_state_db() {
    let path = default_windsurf_db_path().expect("default path");
    let text = path.to_string_lossy();

    assert!(text.contains("Windsurf"));
    assert!(text.ends_with("state.vscdb"));
}

#[test]
fn extract_key_reads_windsurf_auth_status_fixture() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.vscdb");
    let connection = Connection::open(&db_path).expect("open sqlite");
    connection
        .execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .expect("create table");
    connection
        .execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            (
                "windsurfAuthStatus",
                r#"{"apiKey":"ws-test-key-1234567890","user":"fixture"}"#,
            ),
        )
        .expect("insert auth");
    drop(connection);

    let key = extract_key_from_db_path(&db_path).expect("extract key");
    assert_eq!(key, "ws-test-key-1234567890");
}

#[test]
fn extract_key_ignores_unrelated_tokens_in_non_windsurf_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.vscdb");
    let connection = Connection::open(&db_path).expect("open sqlite");
    connection
        .execute("CREATE TABLE SecretTable (token TEXT NOT NULL)", [])
        .expect("create table");
    connection
        .execute(
            "INSERT INTO SecretTable (token) VALUES (?1)",
            ["prefix apiKey: unrelated-column-key-123456"],
        )
        .expect("insert unrelated token");
    drop(connection);

    let error = extract_key_from_db_path(&db_path).expect_err("unrelated token must be ignored");
    assert_eq!(error.kind, FastContextErrorKind::MissingApiKey);
}

#[test]
fn extract_key_prefers_windsurf_specific_records_over_unrelated_tokens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.vscdb");
    let connection = Connection::open(&db_path).expect("open sqlite");
    connection
        .execute("CREATE TABLE SecretTable (token TEXT NOT NULL)", [])
        .expect("create unrelated table");
    connection
        .execute(
            "INSERT INTO SecretTable (token) VALUES (?1)",
            ["prefix apiKey: unrelated-column-key-123456"],
        )
        .expect("insert unrelated token");
    connection
        .execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .expect("create item table");
    connection
        .execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            (
                "windsurfAuthStatus",
                r#"{"apiKey":"ws-preferred-key-1234567890","user":"fixture"}"#,
            ),
        )
        .expect("insert windsurf auth");
    drop(connection);

    let key = extract_key_from_db_path(&db_path).expect("extract key");
    assert_eq!(key, "ws-preferred-key-1234567890");
}

#[test]
fn extract_key_reads_windsurf_named_table_token_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.vscdb");
    let connection = Connection::open(&db_path).expect("open sqlite");
    connection
        .execute("CREATE TABLE WindsurfSecrets (token TEXT NOT NULL)", [])
        .expect("create windsurf table");
    connection
        .execute(
            "INSERT INTO WindsurfSecrets (token) VALUES (?1)",
            ["prefix apiKey: ws-table-key-123456"],
        )
        .expect("insert windsurf token");
    drop(connection);

    let key = extract_key_from_db_path(&db_path).expect("extract key");
    assert_eq!(key, "ws-table-key-123456");
}

#[test]
fn extract_key_returns_missing_api_key_when_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.vscdb");
    let connection = Connection::open(&db_path).expect("open sqlite");
    connection
        .execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .expect("create table");
    drop(connection);

    let error = extract_key_from_db_path(&db_path).expect_err("missing key");
    assert_eq!(error.kind, FastContextErrorKind::MissingApiKey);
}
