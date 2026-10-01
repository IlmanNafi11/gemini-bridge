//! OpenAI tool call data structures.

use serde::{Deserialize, Serialize};

/// An OpenAI-compatible tool call emitted in `choices[0].message.tool_calls`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallObject {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

/// The function invocation details within a [`ToolCallObject`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// JSON-encoded arguments string.
    pub arguments: String,
}

impl ToolCallObject {
    /// Convenience constructor for a function tool call.
    pub fn function(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_object_serializes_openai_shape() {
        let call = ToolCallObject::function("call_abc123", "get_weather", r#"{"city":"London"}"#);
        let val = serde_json::to_value(&call).unwrap();
        assert_eq!(val["id"], "call_abc123");
        assert_eq!(val["type"], "function");
        assert_eq!(val["function"]["name"], "get_weather");
        assert_eq!(val["function"]["arguments"], r#"{"city":"London"}"#);
    }

    #[test]
    fn tool_call_object_deserializes() {
        let json =
            r#"{"id":"call_1","type":"function","function":{"name":"search","arguments":"{}"}}"#;
        let obj: ToolCallObject = serde_json::from_str(json).unwrap();
        assert_eq!(obj.id, "call_1");
        assert_eq!(obj.call_type, "function");
        assert_eq!(obj.function.name, "search");
        assert_eq!(obj.function.arguments, "{}");
    }
}
