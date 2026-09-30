/// Task 0.6 — Schema loading and parser self-check.
///
/// Tests in this file are RED until `schema.rs` gains `GeminiWebSchema::from_toml`
/// and `self_check.rs` gains `SelfCheck::run`.
use gemini_bridge_adapter_gemini::schema::{GeminiWebSchema, PathSegment, SchemaLoadError};
use gemini_bridge_adapter_gemini::self_check::{SelfCheckError, run_self_check};

// ── from_toml: happy-path ───────────────────────────────────────────────────

#[test]
fn from_toml_parses_canonical_schema_file() {
    let src = include_str!("../../../schema/gemini-web.toml");
    let schema = GeminiWebSchema::from_toml(src).expect("canonical schema must parse");

    assert_eq!(schema.user_message, 0);
    assert_eq!(schema.conversation_id, 1);
    assert_eq!(schema.response_id, 2);
    assert_eq!(
        schema.candidate_text_path,
        vec![
            PathSegment::Field("candidates".to_owned()),
            PathSegment::Index(0),
            PathSegment::Field("parts".to_owned()),
            PathSegment::Index(0),
            PathSegment::Field("text".to_owned()),
        ]
    );
}

#[test]
fn from_toml_returns_same_schema_as_default() {
    let src = include_str!("../../../schema/gemini-web.toml");
    let loaded = GeminiWebSchema::from_toml(src).unwrap();
    assert_eq!(loaded, GeminiWebSchema::default());
}

// ── from_toml: error paths ──────────────────────────────────────────────────

#[test]
fn from_toml_rejects_empty_input() {
    let err = GeminiWebSchema::from_toml("").unwrap_err();
    assert!(
        matches!(err, SchemaLoadError::ParseError(_)),
        "expected ParseError, got {err:?}"
    );
}

#[test]
fn from_toml_rejects_missing_positions_table() {
    let src = "[other]\nfoo = 1";
    let err = GeminiWebSchema::from_toml(src).unwrap_err();
    assert!(
        matches!(err, SchemaLoadError::MissingField(_)),
        "expected MissingField, got {err:?}"
    );
}

#[test]
fn from_toml_rejects_negative_index_in_candidate_path() {
    // candidate_text_path alternates type and value; "index" values must be
    // non-negative integers. A value that cannot parse as usize is an error.
    let src = r#"
[positions]
user_message = 0
conversation_id = 1
response_id = 2
candidate_text_path = ["index", "-1"]
"#;
    let err = GeminiWebSchema::from_toml(src).unwrap_err();
    assert!(
        matches!(err, SchemaLoadError::InvalidPath(_)),
        "expected InvalidPath, got {err:?}"
    );
}

#[test]
fn from_toml_rejects_odd_length_candidate_path() {
    // Path must be even-length (type, value) pairs.
    let src = r#"
[positions]
user_message = 0
conversation_id = 1
response_id = 2
candidate_text_path = ["field"]
"#;
    let err = GeminiWebSchema::from_toml(src).unwrap_err();
    assert!(
        matches!(err, SchemaLoadError::InvalidPath(_)),
        "expected InvalidPath, got {err:?}"
    );
}

#[test]
fn from_toml_rejects_unknown_path_segment_type() {
    let src = r#"
[positions]
user_message = 0
conversation_id = 1
response_id = 2
candidate_text_path = ["weird", "0"]
"#;
    let err = GeminiWebSchema::from_toml(src).unwrap_err();
    assert!(
        matches!(err, SchemaLoadError::InvalidPath(_)),
        "expected InvalidPath, got {err:?}"
    );
}

#[test]
fn from_toml_rejects_duplicate_positional_indices() {
    // All three positional indices must be distinct.
    let src = r#"
[positions]
user_message = 0
conversation_id = 0
response_id = 2
candidate_text_path = ["field", "candidates", "index", "0", "field", "parts", "index", "0", "field", "text"]
"#;
    let err = GeminiWebSchema::from_toml(src).unwrap_err();
    assert!(
        matches!(err, SchemaLoadError::InvalidPath(_)),
        "expected InvalidPath, got {err:?}"
    );
}

// ── self-check ──────────────────────────────────────────────────────────────

const SAMPLE_RESPONSE: &str = include_str!("../fixtures/sample_response.txt");

#[test]
fn self_check_passes_with_canonical_schema_against_sample_fixture() {
    let schema = GeminiWebSchema::default();
    run_self_check(&schema, SAMPLE_RESPONSE).expect("self-check must pass against sample fixture");
}

#[test]
fn self_check_returns_error_when_candidate_path_does_not_match_fixture() {
    use gemini_bridge_adapter_gemini::schema::PathSegment;
    let schema = GeminiWebSchema {
        // Deliberately wrong path depth — won't find "text" here.
        candidate_text_path: vec![
            PathSegment::Field("candidates".to_owned()),
            PathSegment::Index(99), // out-of-bounds index
            PathSegment::Field("text".to_owned()),
        ],
        ..GeminiWebSchema::default()
    };
    let err = run_self_check(&schema, SAMPLE_RESPONSE).unwrap_err();
    assert!(
        matches!(err, SelfCheckError::CandidatePathMismatch(_)),
        "expected CandidatePathMismatch, got {err:?}"
    );
}

#[test]
fn self_check_returns_error_on_empty_probe() {
    let schema = GeminiWebSchema::default();
    let err = run_self_check(&schema, "").unwrap_err();
    assert!(
        matches!(err, SelfCheckError::NoFramesParsed),
        "expected NoFramesParsed, got {err:?}"
    );
}

#[test]
fn self_check_returns_error_on_malformed_json_probe() {
    let schema = GeminiWebSchema::default();
    let err = run_self_check(&schema, "not json at all").unwrap_err();
    assert!(
        matches!(err, SelfCheckError::NoFramesParsed),
        "expected NoFramesParsed, got {err:?}"
    );
}
