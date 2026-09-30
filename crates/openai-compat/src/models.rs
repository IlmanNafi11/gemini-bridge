//! OpenAI `/v1/models` endpoint types and model list.

use serde::Serialize;

use crate::unix_now;

/// An entry in the `/v1/models` response.
#[derive(Debug, Clone, Serialize)]
pub struct ModelObject {
    pub id: &'static str,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: &'static str,
}

/// The `/v1/models` list response.
#[derive(Debug, Clone, Serialize)]
pub struct ModelList {
    pub object: &'static str,
    pub data: Vec<ModelObject>,
}

/// All virtual model aliases exposed by gemini-bridge.
pub const VIRTUAL_MODELS: &[&str] = &[
    "gemini-web-flash",
    "gemini-web-pro",
    "gemini-web-thinking",
    "gemini-web-auto",
];

/// Build the models list response.
pub fn list_models() -> ModelList {
    let created = unix_now();
    ModelList {
        object: "list",
        data: VIRTUAL_MODELS
            .iter()
            .map(|&id| ModelObject {
                id,
                object: "model",
                created,
                owned_by: "google",
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_models_contains_expected_ids() {
        let list = list_models();
        let ids: Vec<&str> = list.data.iter().map(|m| m.id).collect();
        assert!(ids.contains(&"gemini-web-flash"));
        assert!(ids.contains(&"gemini-web-pro"));
        assert!(ids.contains(&"gemini-web-thinking"));
        assert!(ids.contains(&"gemini-web-auto"));
    }

    #[test]
    fn model_list_serializes_openai_shape() {
        let list = list_models();
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(json["object"], "list");
        assert!(json["data"].is_array());
        let first = &json["data"][0];
        assert_eq!(first["object"], "model");
        assert!(first["id"].is_string());
    }
}
