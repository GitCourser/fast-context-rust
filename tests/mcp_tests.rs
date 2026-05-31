use std::fs;

use fast_context_rust::mcp::{
    FAST_CONTEXT_TOOL_NAME, call_fast_context_search, handle_json_rpc_message, tools_list_result,
};
use serde_json::json;

#[test]
fn tools_list_only_exposes_fast_context_search() {
    let result = tools_list_result();
    let tools = result["tools"].as_array().expect("tools array");

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], FAST_CONTEXT_TOOL_NAME);
    assert_ne!(tools[0]["name"], "extract_windsurf_key");
}

#[test]
fn initialize_and_tools_list_json_rpc_are_supported() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let initialize = runtime
        .block_on(handle_json_rpc_message(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })))
        .expect("initialize response");
    assert_eq!(initialize["id"], 1);
    assert_eq!(
        initialize["result"]["serverInfo"]["name"],
        "fast-context-rust"
    );

    let tools = runtime
        .block_on(handle_json_rpc_message(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        })))
        .expect("tools/list response");
    assert_eq!(tools["id"], 2);
    assert_eq!(tools["result"]["tools"][0]["name"], FAST_CONTEXT_TOOL_NAME);
}

#[test]
fn tools_call_valid_project_path_returns_structured_error_content_with_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let result = runtime.block_on(call_fast_context_search(&json!({
        "query": "where is auth?",
        "project_path": dir.path().to_string_lossy(),
        "max_turns": 3
    })));

    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().expect("text content");
    assert!(text.contains("[config]"));
    assert!(text.contains("project_path="));
    assert!(
        text.contains("MISSING_API_KEY") || text.contains("NOT_IMPLEMENTED"),
        "unexpected search error text: {text}"
    );
}

#[test]
fn tools_call_rejects_invalid_project_paths_before_search_engine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("not-a-directory.txt");
    fs::write(&file_path, "fixture").expect("write file fixture");

    let missing = json!({ "query": "where is auth?" });
    let empty = json!({ "query": "where is auth?", "project_path": "" });
    let relative = json!({ "query": "where is auth?", "project_path": "relative/project" });
    let nonexistent = json!({
        "query": "where is auth?",
        "project_path": dir.path().join("missing").to_string_lossy()
    });
    let file = json!({
        "query": "where is auth?",
        "project_path": file_path.to_string_lossy()
    });

    let cases = [
        (missing, "project_path is required"),
        (empty, "project_path is required"),
        (relative, "project_path must be an absolute path"),
        (nonexistent, "project_path does not exist"),
        (file, "project_path is not a directory"),
    ];

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    for (args, expected) in cases {
        let result = runtime.block_on(call_fast_context_search(&args));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().expect("text content");
        assert!(text.contains(expected), "expected {expected:?} in {text:?}");
        assert!(
            !text.contains("[config]"),
            "validation errors should be returned before SearchEngine metadata is created: {text}"
        );
    }
}

#[test]
fn unknown_tool_returns_json_rpc_error() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let response = runtime
        .block_on(handle_json_rpc_message(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "extract_windsurf_key", "arguments": {} }
        })))
        .expect("error response");

    assert_eq!(response["id"], 9);
    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("only fast_context_search is supported")
    );
}
