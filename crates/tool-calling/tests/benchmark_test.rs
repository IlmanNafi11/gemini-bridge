use gemini_bridge_llm_service::ToolDefinition;
use gemini_bridge_tool_calling::{DefaultToolEngine, ParsedToolResult, ToolEngine};
use serde_json::json;

fn registered_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "get_weather".into(),
            description: "Get current weather in a given city".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" },
                    "unit": { "type": "string" }
                },
                "required": ["location"]
            }),
        },
        ToolDefinition {
            name: "calculate".into(),
            description: "Perform math calculation".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number" },
                    "factor": { "type": "number" }
                },
                "required": ["x"]
            }),
        },
        ToolDefinition {
            name: "toggle_feature".into(),
            description: "Enable or disable a feature".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "feature_name": { "type": "string" },
                    "enabled": { "type": "boolean" }
                },
                "required": ["feature_name", "enabled"]
            }),
        },
        ToolDefinition {
            name: "search".into(),
            description: "Search knowledge base".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "filter": { "type": "string" }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "batch_tag".into(),
            description: "Assign tags to resource".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["tags"]
            }),
        },
        ToolDefinition {
            name: "compute_stats".into(),
            description: "Compute statistical metrics".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "values": {
                        "type": "array",
                        "items": { "type": "number" }
                    }
                },
                "required": ["values"]
            }),
        },
        ToolDefinition {
            name: "get_current_time".into(),
            description: "Get server time".into(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "send_email".into(),
            description: "Send email message".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string" },
                    "subject": { "type": "string" },
                    "body": { "type": "string" }
                },
                "required": ["to", "subject"]
            }),
        },
        ToolDefinition {
            name: "execute_query".into(),
            description: "Execute read-only SQL query".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "sql": { "type": "string" }
                },
                "required": ["sql"]
            }),
        },
        ToolDefinition {
            name: "write_file".into(),
            description: "Write content to a file".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDefinition {
            name: "fetch_url".into(),
            description: "Fetch contents from HTTP URL".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" }
                },
                "required": ["url"]
            }),
        },
        ToolDefinition {
            name: "get_stock_price".into(),
            description: "Get current stock price".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "ticker": { "type": "string" }
                },
                "required": ["ticker"]
            }),
        },
        ToolDefinition {
            name: "scientific_calc".into(),
            description: "Perform scientific calculations".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "val": { "type": "number" },
                    "power": { "type": "number" }
                },
                "required": ["val"]
            }),
        },
        ToolDefinition {
            name: "create_user".into(),
            description: "Create new user account".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "user": { "type": "object" }
                },
                "required": ["user"]
            }),
        },
        ToolDefinition {
            name: "bulk_insert".into(),
            description: "Bulk insert data items".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": { "type": "array" }
                },
                "required": ["items"]
            }),
        },
        ToolDefinition {
            name: "process_tree".into(),
            description: "Process nested tree node".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "root": { "type": "object" }
                },
                "required": ["root"]
            }),
        },
        ToolDefinition {
            name: "configure_service".into(),
            description: "Configure service routing and options".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string" },
                    "routes": { "type": "array" },
                    "options": { "type": "object" }
                },
                "required": ["service"]
            }),
        },
        ToolDefinition {
            name: "matrix_transform".into(),
            description: "Transform numerical matrix".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "matrix": { "type": "array" },
                    "labels": { "type": "array" }
                },
                "required": ["matrix"]
            }),
        },
    ]
}

enum ExpectedOutcome {
    Calls {
        count: usize,
        function_names: Vec<&'static str>,
        has_text_prefix: bool,
    },
    FallbackWithWarning,
}

struct BenchmarkCase {
    id: usize,
    category: &'static str,
    description: &'static str,
    input: String,
    expected: ExpectedOutcome,
}

fn generate_50_benchmark_cases() -> Vec<BenchmarkCase> {
    vec![
        // ── Category 1: Single-function valid calls (20 cases: 1..=20) ─────────
        BenchmarkCase {
            id: 1,
            category: "Single Valid Call",
            description: "Fenced markdown JSON with simple string parameter",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"San Francisco, CA\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 2,
            category: "Single Valid Call",
            description: "Fenced markdown JSON with integer and float parameters",
            input: "```json\n{\"name\": \"calculate\", \"arguments\": {\"x\": 42, \"factor\": 3.14}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["calculate"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 3,
            category: "Single Valid Call",
            description: "Fenced markdown JSON with boolean parameter",
            input: "```json\n{\"name\": \"toggle_feature\", \"arguments\": {\"feature_name\": \"dark_mode\", \"enabled\": true}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["toggle_feature"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 4,
            category: "Single Valid Call",
            description: "Fenced markdown JSON with null optional parameter",
            input: "```json\n{\"name\": \"search\", \"arguments\": {\"query\": \"rust async\", \"filter\": null}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["search"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 5,
            category: "Single Valid Call",
            description: "Fenced markdown JSON with string array parameter",
            input: "```json\n{\"name\": \"batch_tag\", \"arguments\": {\"tags\": [\"rust\", \"ai\", \"gemini\"]}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["batch_tag"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 6,
            category: "Single Valid Call",
            description: "Fenced markdown JSON with number array parameter",
            input: "```json\n{\"name\": \"compute_stats\", \"arguments\": {\"values\": [1.0, 2.5, 3.8, 4.2]}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["compute_stats"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 7,
            category: "Single Valid Call",
            description: "Empty object arguments for parameterless tool",
            input: "```json\n{\"name\": \"get_current_time\", \"arguments\": {}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_current_time"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 8,
            category: "Single Valid Call",
            description: "Pre-encoded string arguments in fenced JSON",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": \"{\\\"location\\\":\\\"Tokyo\\\"}\"}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 9,
            category: "Single Valid Call",
            description: "Raw un-fenced top-level JSON object",
            input: "{\"name\": \"send_email\", \"arguments\": {\"to\": \"alice@example.com\", \"subject\": \"Meeting notes\"}}".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["send_email"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 10,
            category: "Single Valid Call",
            description: "XML-style tool_call tag with single call",
            input: "<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"location\": \"London\"}}</tool_call>".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 11,
            category: "Single Valid Call",
            description: "Generic code fence without language tag",
            input: "```\n{\"name\": \"execute_query\", \"arguments\": {\"sql\": \"SELECT * FROM users WHERE active = 1;\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["execute_query"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 12,
            category: "Single Valid Call",
            description: "Tool call with multiple scalar properties populated",
            input: "```json\n{\"name\": \"send_email\", \"arguments\": {\"to\": \"ops@company.com\", \"subject\": \"Alert\", \"body\": \"Disk usage is high\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["send_email"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 13,
            category: "Single Valid Call",
            description: "Tool call with only required properties provided (optional omitted)",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Sydney\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 14,
            category: "Single Valid Call",
            description: "Tool call with escaped newlines and internal quotes in argument string",
            input: "```json\n{\"name\": \"write_file\", \"arguments\": {\"path\": \"output/log.txt\", \"content\": \"Line 1\\nLine 2\\t\\\"quoted value\\\"\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["write_file"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 15,
            category: "Single Valid Call",
            description: "URL and query parameters containing special characters",
            input: "```json\n{\"name\": \"fetch_url\", \"arguments\": {\"url\": \"https://api.example.com/v2/items?category=books&limit=50#section-3\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["fetch_url"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 16,
            category: "Single Valid Call",
            description: "Tool call with both required and optional parameters supplied",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Paris, FR\", \"unit\": \"celsius\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 17,
            category: "Single Valid Call",
            description: "Tool call with float multiplier factor parameter",
            input: "```json\n{\"name\": \"calculate\", \"arguments\": {\"x\": 100.0, \"factor\": 0.25}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["calculate"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 18,
            category: "Single Valid Call",
            description: "Tool call for stock price lookup",
            input: "```json\n{\"name\": \"get_stock_price\", \"arguments\": {\"ticker\": \"GOOGL\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_stock_price"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 19,
            category: "Single Valid Call",
            description: "Tool call surrounded by leading and trailing empty lines and tabs",
            input: "\n\n\t```json\n\t{\"name\": \"get_current_time\", \"arguments\": {}}\n\t```\n\n".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_current_time"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 20,
            category: "Single Valid Call",
            description: "Scientific notation and negative numeric arguments",
            input: "```json\n{\"name\": \"scientific_calc\", \"arguments\": {\"val\": -1.25e-4, \"power\": -3.0}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["scientific_calc"],
                has_text_prefix: false,
            },
        },

        // ── Category 2: Multi-function calls in single response (10 cases: 21..=30)
        BenchmarkCase {
            id: 21,
            category: "Multi-function Calls",
            description: "JSON array with two identical tool calls with different arguments",
            input: "```json\n[\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Berlin\"}},\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Munich\"}}\n]\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["get_weather", "get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 22,
            category: "Multi-function Calls",
            description: "JSON array with three distinct tool calls",
            input: "```json\n[\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Rome\"}},\n  {\"name\": \"get_current_time\", \"arguments\": {}},\n  {\"name\": \"calculate\", \"arguments\": {\"x\": 10, \"factor\": 2.0}}\n]\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 3,
                function_names: vec!["get_weather", "get_current_time", "calculate"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 23,
            category: "Multi-function Calls",
            description: "Multiple XML <tool_call> tags in sequence",
            input: "<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Toronto\"}}</tool_call>\n<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Vancouver\"}}</tool_call>".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["get_weather", "get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 24,
            category: "Multi-function Calls",
            description: "Raw un-fenced JSON array containing two tool calls",
            input: "[{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Kyoto\"}}, {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Osaka\"}}]".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["get_weather", "get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 25,
            category: "Multi-function Calls",
            description: "Array of calls with varied scalar and array parameter schemas",
            input: "```json\n[\n  {\"name\": \"toggle_feature\", \"arguments\": {\"feature_name\": \"beta_v2\", \"enabled\": false}},\n  {\"name\": \"batch_tag\", \"arguments\": {\"tags\": [\"v2\", \"staging\"]}}\n]\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["toggle_feature", "batch_tag"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 26,
            category: "Multi-function Calls",
            description: "Array of calls using pre-encoded JSON strings for arguments",
            input: "```json\n[\n  {\"name\": \"get_weather\", \"arguments\": \"{\\\"location\\\":\\\"Amsterdam\\\"}\"},\n  {\"name\": \"get_weather\", \"arguments\": \"{\\\"location\\\":\\\"Rotterdam\\\"}\"}\n]\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["get_weather", "get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 27,
            category: "Multi-function Calls",
            description: "Four sequential tool calls in a single JSON array",
            input: "```json\n[\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Oslo\"}},\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Stockholm\"}},\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Helsinki\"}},\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Copenhagen\"}}\n]\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 4,
                function_names: vec!["get_weather", "get_weather", "get_weather", "get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 28,
            category: "Multi-function Calls",
            description: "Array of tool calls with line breaks and varied indentation",
            input: "[\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Vienna\"}},\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Salzburg\"}}\n]".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["get_weather", "get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 29,
            category: "Multi-function Calls",
            description: "Array of distinct calls in fenced code block",
            input: "```json\n[\n  {\"name\": \"get_current_time\", \"arguments\": {}},\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Dublin\"}}\n]\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["get_current_time", "get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 30,
            category: "Multi-function Calls",
            description: "Multiple parameterless tool calls in an array",
            input: "```json\n[\n  {\"name\": \"get_current_time\", \"arguments\": {}},\n  {\"name\": \"get_current_time\", \"arguments\": {}}\n]\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 2,
                function_names: vec!["get_current_time", "get_current_time"],
                has_text_prefix: false,
            },
        },

        // ── Category 3: Nested object / complex array schema calls (5 cases: 31..=35)
        BenchmarkCase {
            id: 31,
            category: "Complex / Nested Schemas",
            description: "Deeply nested object parameter",
            input: "```json\n{\"name\": \"create_user\", \"arguments\": {\"user\": {\"name\": \"Alice\", \"address\": {\"city\": \"Boston\", \"zip\": 12345, \"coords\": {\"lat\": 42.36, \"lng\": -71.05}}}}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["create_user"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 32,
            category: "Complex / Nested Schemas",
            description: "Array of complex objects parameter",
            input: "```json\n{\"name\": \"bulk_insert\", \"arguments\": {\"items\": [{\"id\": 101, \"title\": \"Alpha\", \"active\": true}, {\"id\": 102, \"title\": \"Beta\", \"active\": false}]}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["bulk_insert"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 33,
            category: "Complex / Nested Schemas",
            description: "Recursive-like tree node structure in arguments",
            input: "```json\n{\"name\": \"process_tree\", \"arguments\": {\"root\": {\"val\": 1, \"left\": {\"val\": 2, \"left\": null, \"right\": null}, \"right\": {\"val\": 3, \"left\": null, \"right\": null}}}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["process_tree"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 34,
            category: "Complex / Nested Schemas",
            description: "Mixed configuration schema with arrays, objects, and strings",
            input: "```json\n{\"name\": \"configure_service\", \"arguments\": {\"service\": \"ingress-gateway\", \"routes\": [{\"path\": \"/api/v1\", \"methods\": [\"GET\", \"POST\"]}], \"options\": {\"timeout_ms\": 5000, \"retries\": 3, \"enable_tls\": true}}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["configure_service"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 35,
            category: "Complex / Nested Schemas",
            description: "2D nested numerical matrix array with string labels",
            input: "```json\n{\"name\": \"matrix_transform\", \"arguments\": {\"matrix\": [[1.0, 0.0, 0.5], [0.0, 1.0, 0.2]], \"labels\": [\"x\", \"y\"]}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["matrix_transform"],
                has_text_prefix: false,
            },
        },

        // ── Category 4: Calls wrapped in prose / explanatory text (5 cases: 36..=40)
        BenchmarkCase {
            id: 36,
            category: "Prose + Tool Call",
            description: "Explanatory conversational sentence before fenced tool call",
            input: "I will check the current weather for Seattle right now.\n```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Seattle, WA\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: true,
            },
        },
        BenchmarkCase {
            id: 37,
            category: "Prose + Tool Call",
            description: "Short conversational intro before raw un-fenced JSON object",
            input: "Certainly! Here is the function invocation:\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Chicago, IL\"}}".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: true,
            },
        },
        BenchmarkCase {
            id: 38,
            category: "Prose + Tool Call",
            description: "Conversational prefix before XML <tool_call> tag",
            input: "Let me query that data for you:\n<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Austin, TX\"}}</tool_call>".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: true,
            },
        },
        BenchmarkCase {
            id: 39,
            category: "Prose + Tool Call",
            description: "Reasoning and chain-of-thought paragraph before fenced call",
            input: "Thinking Process:\n1. The user wants to calculate 16 * 0.5\n2. I should use calculate tool.\n\n```json\n{\"name\": \"calculate\", \"arguments\": {\"x\": 16, \"factor\": 0.5}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["calculate"],
                has_text_prefix: true,
            },
        },
        BenchmarkCase {
            id: 40,
            category: "Prose + Tool Call",
            description: "Multi-line greeting and preamble before email sending tool call",
            input: "Hello! I can send that email on your behalf.\nPreparing email message payload...\n```json\n{\"name\": \"send_email\", \"arguments\": {\"to\": \"dev@example.com\", \"subject\": \"Status Report\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["send_email"],
                has_text_prefix: true,
            },
        },

        // ── Category 5: Malformed JSON cases (Safe Fallbacks) (5 cases: 41..=45) ─
        BenchmarkCase {
            id: 41,
            category: "Malformed / Fallback",
            description: "Unclosed curly brace in fenced JSON block",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Miami\"\n```".into(),
            expected: ExpectedOutcome::FallbackWithWarning,
        },
        BenchmarkCase {
            id: 42,
            category: "Malformed / Fallback",
            description: "Trailing comma creating invalid JSON syntax",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Dallas\",}}\n```".into(),
            expected: ExpectedOutcome::FallbackWithWarning,
        },
        BenchmarkCase {
            id: 43,
            category: "Malformed / Fallback",
            description: "Fenced JSON block with unclosed quote in argument",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Los Angeles\n```".into(),
            expected: ExpectedOutcome::FallbackWithWarning,
        },
        BenchmarkCase {
            id: 44,
            category: "Malformed / Fallback",
            description: "Malformed pre-encoded JSON string inside arguments",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": \"{unquoted_location: Miami}\"}\n```".into(),
            expected: ExpectedOutcome::FallbackWithWarning,
        },
        BenchmarkCase {
            id: 45,
            category: "Malformed / Fallback",
            description: "Completely broken syntax resembling a tool call attempt",
            input: "```json\n{name: get_weather, arguments: broken_payload}\n```".into(),
            expected: ExpectedOutcome::FallbackWithWarning,
        },

        // ── Category 6: Edge Cases & Boundary Values (5 cases: 46..=50) ────────
        BenchmarkCase {
            id: 46,
            category: "Edge Cases",
            description: "Empty arguments object for tool with optional parameters",
            input: "```json\n{\"name\": \"get_current_time\", \"arguments\": {}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_current_time"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 47,
            category: "Edge Cases",
            description: "Unicode characters, emojis, and non-ASCII scripts in arguments",
            input: "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"東京 (Tokyo) 🗼\", \"unit\": \"℃\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 48,
            category: "Edge Cases",
            description: "Extreme string length (5000+ characters) in argument value",
            input: {
                let long_text = "A".repeat(5000);
                format!("```json\n{{\"name\": \"write_file\", \"arguments\": {{\"path\": \"dump.txt\", \"content\": \"{}\"}}}}\n```", long_text)
            },
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["write_file"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 49,
            category: "Edge Cases",
            description: "Excessive whitespace, tabs, and multi-line breaks within JSON payload",
            input: "```json\n{\n\t\t\"name\"  :   \t\n \"get_weather\"  ,\n\n  \"arguments\" \t : \n  {\n\t\"location\"\n : \n\"Geneva\"\n}\n}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["get_weather"],
                has_text_prefix: false,
            },
        },
        BenchmarkCase {
            id: 50,
            category: "Edge Cases",
            description: "Special escaped sequences: backslashes, quotes, and unicode escapes \\u00A9",
            input: "```json\n{\"name\": \"write_file\", \"arguments\": {\"path\": \"C:\\\\Users\\\\Admin\\\\file.txt\", \"content\": \"Copyright \\u00A9 2026 \\\"Company\\\"\\\\All rights reserved.\"}}\n```".into(),
            expected: ExpectedOutcome::Calls {
                count: 1,
                function_names: vec!["write_file"],
                has_text_prefix: false,
            },
        },
    ]
}

#[test]
fn benchmark_50_case_suite_meets_or_exceeds_95_percent_threshold() {
    let engine = DefaultToolEngine;
    let tools = registered_tools();
    let cases = generate_50_benchmark_cases();

    assert_eq!(cases.len(), 50, "Benchmark must have exactly 50 cases");

    let mut passed_count = 0;
    let mut failures: Vec<String> = Vec::new();

    for case in &cases {
        let result = engine.parse_and_validate(&case.input, &tools);

        let case_passed = match (&case.expected, result) {
            (
                ExpectedOutcome::Calls {
                    count,
                    function_names,
                    has_text_prefix,
                },
                ParsedToolResult::ToolCalls { calls, text_prefix },
            ) => {
                let count_ok = calls.len() == *count;
                let names_ok = calls
                    .iter()
                    .zip(function_names.iter())
                    .all(|(c, expected_name)| c.name == *expected_name);
                let args_json_ok = calls
                    .iter()
                    .all(|c| serde_json::from_str::<serde_json::Value>(&c.arguments).is_ok());
                let prefix_ok = if *has_text_prefix {
                    text_prefix.is_some() && !text_prefix.unwrap().trim().is_empty()
                } else {
                    true
                };

                count_ok && names_ok && args_json_ok && prefix_ok
            }
            (
                ExpectedOutcome::FallbackWithWarning,
                ParsedToolResult::PlainContent { content, warning },
            ) => {
                let content_intact = content == case.input;
                let warning_present = warning.is_some();
                content_intact && warning_present
            }
            _ => false,
        };

        if case_passed {
            passed_count += 1;
        } else {
            failures.push(format!(
                "Case #{:02} [{}] '{}' failed",
                case.id, case.category, case.description
            ));
        }
    }

    let success_rate = (passed_count as f64) / (cases.len() as f64);
    println!(
        "50-Case Golden Benchmark Result: {}/{} passed ({:.1}%)",
        passed_count,
        cases.len(),
        success_rate * 100.0
    );

    if !failures.is_empty() {
        for f in &failures {
            eprintln!("  FAILED: {}", f);
        }
    }

    assert!(
        success_rate >= 0.95,
        "Benchmark success rate {:.1}% is below required >=95% (passed {}/50, failures: {:?})",
        success_rate * 100.0,
        passed_count,
        failures
    );
    assert_eq!(
        passed_count, 50,
        "All 50 benchmark test cases must pass deterministically"
    );
}
