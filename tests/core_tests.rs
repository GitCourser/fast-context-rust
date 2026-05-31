use fast_context_rust::core::{
    SearchConfig, SearchEngine, jwt_exp_unix, parse_answer, parse_connect_response,
    parse_streaming_response, parse_tool_call,
};
use fast_context_rust::error::FastContextErrorKind;
use fast_context_rust::protobuf::{ProtobufEncoder, connect_frame_encode};

#[test]
fn parse_tool_call_supports_restricted_exec_and_lenient_json() {
    let text = r#"I should inspect code.
[TOOL_CALLS]restricted_exec[ARGS]{command1:{type:"rg",pattern:"foo",path:"/codebase"}}"#;
    let parsed = parse_tool_call(text).expect("tool call");

    assert_eq!(parsed.thinking, "I should inspect code.");
    assert_eq!(parsed.name, "restricted_exec");
    assert_eq!(parsed.args["command1"]["type"], "rg");
    assert_eq!(parsed.args["command1"]["pattern"], "foo");
}

#[test]
fn parse_tool_call_supports_real_windsurf_format_without_args_marker() {
    let text = r#"I need to inspect the Rust implementation.[TOOL_CALLS]restricted_exec {"command1":{"type":"rg","pattern":"SearchEngine","path":"/codebase/src","exclude":["tests","test"]},"command2":{"type":"readfile","file":"/codebase/src/core.rs"}}</s>ignored trailing text"#;
    let parsed = parse_tool_call(text).expect("tool call");

    assert_eq!(
        parsed.thinking,
        "I need to inspect the Rust implementation."
    );
    assert_eq!(parsed.name, "restricted_exec");
    assert_eq!(parsed.args["command1"]["type"], "rg");
    assert_eq!(parsed.args["command1"]["exclude"][0], "tests");
    assert_eq!(parsed.args["command2"]["file"], "/codebase/src/core.rs");
}

#[test]
fn parse_tool_call_supports_windsurf_shorthand_and_trailing_commas() {
    let text = r#"Searching now.[TOOL_CALLS]restricted_exec {"command1":{"rg":"SearchEngine.*search","path":"/codebase/src","exclude":["tests", "test",],},"command2":{"readfile":{"file":"/codebase/src/core.rs","start_line":190,"end_line":285,},},}</s>ignored"#;
    let parsed = parse_tool_call(text).expect("tool call");

    assert_eq!(parsed.thinking, "Searching now.");
    assert_eq!(parsed.name, "restricted_exec");
    assert_eq!(parsed.args["command1"]["type"], "rg");
    assert_eq!(parsed.args["command1"]["pattern"], "SearchEngine.*search");
    assert_eq!(parsed.args["command1"]["path"], "/codebase/src");
    assert_eq!(parsed.args["command1"]["exclude"][1], "test");
    assert_eq!(parsed.args["command2"]["type"], "readfile");
    assert_eq!(parsed.args["command2"]["file"], "/codebase/src/core.rs");
    assert_eq!(parsed.args["command2"]["start_line"], 190);
}

#[test]
fn parse_tool_call_promotes_nested_commands_from_malformed_windsurf_args() {
    let text = r#"I need to inspect the architecture.[TOOL_CALLS]restricted_exec {"command1":{"rg":{"pattern":"MCP|mcp","path":"/codebase","exclude":["tests","test"]},"command2":{"readfile":{"file":"/codebase/fast-context-rust/src/main.rs"}},"command3":{"readfile":{"file":"/codebase/fast-context-rust/src/core.rs","start_line":190,"end_line":280}}}}</s>ignored"#;
    let parsed = parse_tool_call(text).expect("tool call");

    assert_eq!(parsed.name, "restricted_exec");
    assert_eq!(parsed.args["command1"]["type"], "rg");
    assert_eq!(parsed.args["command1"]["pattern"], "MCP|mcp");
    assert_eq!(parsed.args["command2"]["type"], "readfile");
    assert_eq!(
        parsed.args["command2"]["file"],
        "/codebase/fast-context-rust/src/main.rs"
    );
    assert_eq!(parsed.args["command3"]["type"], "readfile");
    assert_eq!(parsed.args["command3"]["start_line"], 190);
}

#[test]
fn parse_tool_call_repairs_unclosed_object_args() {
    let text = r#"Inspect now.[TOOL_CALLS]restricted_exec {"command1":{"readfile":{"file":"/codebase/fast-context-rust/src/core.rs"}}"#;
    let parsed = parse_tool_call(text).expect("tool call");

    assert_eq!(parsed.name, "restricted_exec");
    assert_eq!(parsed.args["command1"]["type"], "readfile");
    assert_eq!(
        parsed.args["command1"]["file"],
        "/codebase/fast-context-rust/src/core.rs"
    );
}

#[test]
fn parse_tool_call_ignores_trailing_text_after_complete_json() {
    let text = r#"[TOOL_CALLS]answer[ARGS]{"answer":"<ANSWER></ANSWER>"}\nextra model text"#;
    let parsed = parse_tool_call(text).expect("tool call");

    assert_eq!(parsed.name, "answer");
    assert_eq!(parsed.args["answer"], "<ANSWER></ANSWER>");
}

#[test]
fn parse_tool_call_rejects_malformed_args() {
    assert!(parse_tool_call("[TOOL_CALLS]restricted_exec[ARGS]{not-json").is_none());
}

#[test]
fn parse_answer_extracts_self_closing_ranges_and_rejects_traversal() {
    let parsed = parse_answer(
        r#"<answer>
            <file path="/codebase/src/a.rs" start="1" end="5" />
            <file path="/codebase/../../etc/passwd" start="1" end="1" />
            <file path="docs/guide.md"><range>7-9</range><range>20:21</range></file>
        </answer>"#,
    );

    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].path, "src/a.rs");
    assert_eq!((parsed[0].start, parsed[0].end), (1, 5));
    assert_eq!(parsed[1].path, "docs/guide.md");
    assert_eq!((parsed[1].start, parsed[1].end), (7, 9));
    assert_eq!((parsed[2].start, parsed[2].end), (20, 21));
}

#[test]
fn parse_connect_response_extracts_direct_and_protobuf_strings() {
    let direct = connect_frame_encode(b"[TOOL_CALLS]restricted_exec[ARGS]{}", false).unwrap();
    assert_eq!(
        parse_connect_response(&direct).expect("direct"),
        vec!["[TOOL_CALLS]restricted_exec[ARGS]{}".to_string()]
    );

    let proto = ProtobufEncoder::new()
        .write_string(1, "This is a sufficiently long final text payload.")
        .to_vec();
    let frame = connect_frame_encode(&proto, false).unwrap();
    assert_eq!(
        parse_connect_response(&frame).expect("protobuf"),
        vec!["This is a sufficiently long final text payload.".to_string()]
    );
}

#[test]
fn search_engine_metadata_and_missing_api_key_are_stable() {
    let mut config = SearchConfig::new("/tmp/example-project");
    config.api_key = None;
    let engine = SearchEngine::new(config);
    let metadata = engine.metadata();

    assert!(metadata.starts_with("[config]"));
    assert!(metadata.contains("api_key=missing"));

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let error = runtime
        .block_on(engine.search("where is auth implemented?"))
        .expect_err("missing key should error");
    assert_eq!(error.kind, FastContextErrorKind::MissingApiKey);
    assert!(error.user_message().contains("WINDSURF_API_KEY"));
}

#[test]
fn search_engine_with_invalid_api_key_attempts_auth_instead_of_not_implemented() {
    let mut config = SearchConfig::new("/tmp/example-project");
    config.api_key = Some("ws-test-key-present".to_string());
    config.timeout_ms = 1;
    let engine = SearchEngine::new(config);

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let error = runtime
        .block_on(engine.search("where is auth implemented?"))
        .expect_err("invalid key or tiny timeout should fail before search succeeds");
    assert_ne!(error.kind, FastContextErrorKind::NotImplemented);
}

#[test]
fn jwt_exp_unix_parses_base64url_payload() {
    let jwt = "eyJhbGciOiJub25lIn0.eyJleHAiOjEyMzQ1fQ.";
    assert_eq!(jwt_exp_unix(jwt), Some(12345));
}

#[test]
fn parse_streaming_response_recombines_tool_calls_split_across_frames() {
    // Simulates a real Devstral stream where the JSON args are cut mid-string between frames.
    // The first frame ends with an unterminated value `...\"type\":\"re` and the second frame
    // carries the rest of the call. Pre-fix, the parser would break on the first frame,
    // call parse_tool_call on incomplete JSON, fail, and surface a "raw_response truncated" fallback.
    let head =
        b"thinking[TOOL_CALLS]restricted_exec[ARGS]{\"command1\":{\"file\":\"/codebase/src/core.rs\",\"type\":\"re";
    let tail = b"adfile\"}}";
    let mut combined = Vec::new();
    combined.extend_from_slice(&connect_frame_encode(head, false).unwrap());
    combined.extend_from_slice(&connect_frame_encode(tail, false).unwrap());

    let parsed = parse_streaming_response(&combined).expect("parse ok");
    let tool_call = parsed.tool_call.expect("should recombine into a tool call");
    assert_eq!(tool_call.name, "restricted_exec");
    assert_eq!(tool_call.args["command1"]["type"], "readfile");
    assert_eq!(tool_call.args["command1"]["file"], "/codebase/src/core.rs");
    assert_eq!(tool_call.thinking, "thinking");
}

#[test]
fn parse_streaming_response_prefers_protobuf_strings_for_tool_calls() {
    // Real Windsurf frames can wrap the assistant text in protobuf fields. Raw UTF-8
    // decoding keeps field tags/length bytes in the text; if those bytes fall inside
    // the JSON args, parse_tool_call can fail and SearchEngine falls back to
    // "No relevant files found.\n\nRaw response:" even though a tool call is present.
    let frame = ProtobufEncoder::new()
        .write_string(1, "Now inspect.[TOOL_CALLS]restricted_exec[ARGS]")
        .write_string(2, "{\"command1\":{")
        .write_string(
            3,
            "\"file\":\"/codebase/src/core.rs\",\"type\":\"readfile\"}}",
        )
        .to_vec();
    let encoded = connect_frame_encode(&frame, false).unwrap();

    let parsed = parse_streaming_response(&encoded).expect("parse ok");
    assert!(parsed.text.contains("[TOOL_CALLS]restricted_exec"));
    let tool_call = parsed
        .tool_call
        .expect("protobuf string chunks should parse into a tool call");
    assert_eq!(tool_call.name, "restricted_exec");
    assert_eq!(tool_call.args["command1"]["type"], "readfile");
    assert_eq!(tool_call.args["command1"]["file"], "/codebase/src/core.rs");
    assert_eq!(tool_call.thinking, "Now inspect.");
}
