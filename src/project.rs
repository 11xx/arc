//! Small JSON projections for commands whose full reports are intentionally rich.

use serde_json::{Map, Value};

/// Return the value at a dotted object-key/array-index path.
pub fn get(value: Value, path: &str) -> Option<Value> {
    path.split('.').try_fold(value, |value, part| match value {
        Value::Object(values) => values.get(part).cloned(),
        Value::Array(values) => part
            .parse::<usize>()
            .ok()
            .and_then(|index| values.get(index).cloned()),
        _ => None,
    })
}

/// Return a top-level object containing the requested comma-separated fields.
pub fn fields(value: Value, fields: &str) -> Value {
    let Value::Object(values) = value else {
        return Value::Object(Map::new());
    };
    let selected = fields
        .split(',')
        .filter_map(|field| {
            values
                .get(field)
                .cloned()
                .map(|value| (field.to_string(), value))
        })
        .collect();
    Value::Object(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gets_nested_object_values() {
        let value = json!({"claim": {"owner": {"actor": "Ada"}}});
        assert_eq!(get(value, "claim.owner.actor"), Some(json!("Ada")));
    }

    #[test]
    fn gets_array_indices() {
        let value = json!({"gates": [{"result": "pass"}]});
        assert_eq!(get(value, "gates.0.result"), Some(json!("pass")));
    }

    #[test]
    fn missing_path_returns_none() {
        assert_eq!(get(json!({"claim": null}), "claim.owner"), None);
    }
}
