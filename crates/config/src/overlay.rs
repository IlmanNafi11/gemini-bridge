use toml::Value;

use crate::ConfigError;

pub(crate) fn merge(base: &mut Value, overlay: Value) -> Result<(), ConfigError> {
    match (base, overlay) {
        (Value::Table(base), Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(current) = base.get_mut(&key) {
                    merge(current, value)?;
                } else {
                    base.insert(key, value);
                }
            }
            Ok(())
        }
        (base, overlay) if same_kind(base, &overlay) => {
            *base = overlay;
            Ok(())
        }
        (base, overlay) => Err(ConfigError::ValidationError(format!(
            "overlay changes configuration value type from {} to {}",
            kind(base),
            kind(&overlay)
        ))),
    }
}

fn same_kind(left: &Value, right: &Value) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "boolean",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Table(_) => "table",
    }
}
