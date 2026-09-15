//! Signal K schema support for the mapping editor.
//!
//! The schema itself comes from the upstream Signal K specification. This
//! module interprets that schema; it does not contain MasterBus device-specific
//! mapping knowledge.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const ELECTRICAL_SCHEMA: &str = include_str!("signalk-schema/electrical.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignalKField {
    pub path: String,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub pattern: Option<String>,
}

/// Return the electrical device types defined by the bundled Signal K schema.
pub fn electrical_types() -> Vec<String> {
    let schema = schema();

    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };

    properties.keys().cloned().collect()
}

/// Return the Signal K fields available beneath an electrical device type.
///
/// Local `#/definitions/...` references and nested properties are resolved
/// directly from the bundled upstream schema.
pub fn electrical_fields(device_type: &str) -> Vec<SignalKField> {
    let schema = schema();

    let Some(device) = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|p| p.get(device_type))
    else {
        return Vec::new();
    };

    let Some(instance) = device
        .get("patternProperties")
        .and_then(Value::as_object)
        .and_then(|p| p.values().next())
    else {
        return Vec::new();
    };

    let mut fields = BTreeMap::new();
    collect_object_fields(&schema, instance, "", &mut fields);

    fields.into_values().collect()
}

fn schema() -> Value {
    serde_json::from_str(ELECTRICAL_SCHEMA).expect("bundled Signal K electrical schema")
}

fn collect_object_fields(
    schema: &Value,
    node: &Value,
    prefix: &str,
    fields: &mut BTreeMap<String, SignalKField>,
) {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        if let Some(target) = resolve_local_ref(schema, reference) {
            collect_object_fields(schema, target, prefix, fields);
        }
    }

    if let Some(all_of) = node.get("allOf").and_then(Value::as_array) {
        for part in all_of {
            collect_object_fields(schema, part, prefix, fields);
        }
    }

    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            collect_property(schema, name, property, prefix, fields);
        }
    }

    if let Some(pattern_properties) = node.get("patternProperties").and_then(Value::as_object) {
        for (pattern, property) in pattern_properties {
            collect_pattern_property(schema, pattern, property, prefix, fields);
        }
    }
}

fn collect_property(
    schema: &Value,
    name: &str,
    property: &Value,
    prefix: &str,
    fields: &mut BTreeMap<String, SignalKField>,
) {
    if is_identity_field(name) {
        return;
    }

    let path = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    };

    if is_container(schema, property) {
        collect_object_fields(schema, property, &path, fields);
    } else {
        fields.insert(
            path.clone(),
            SignalKField {
                path,
                unit: property
                    .get("units")
                    .or_else(|| property.get("unit"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                description: property
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                pattern: None,
            },
        );
    }
}

fn collect_pattern_property(
    schema: &Value,
    pattern: &str,
    property: &Value,
    prefix: &str,
    fields: &mut BTreeMap<String, SignalKField>,
) {
    let dynamic_prefix = if prefix.is_empty() {
        "{key}".to_owned()
    } else {
        format!("{prefix}.{{key}}")
    };

    let mut dynamic_fields = BTreeMap::new();
    collect_object_fields(schema, property, &dynamic_prefix, &mut dynamic_fields);

    for (_, mut field) in dynamic_fields {
        field.pattern = Some(pattern.to_owned());
        fields.insert(field.path.clone(), field);
    }
}

fn is_container(schema: &Value, node: &Value) -> bool {
    if node.get("properties").and_then(Value::as_object).is_some()
        || node
            .get("patternProperties")
            .and_then(Value::as_object)
            .is_some()
    {
        return true;
    }

    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        if reference.starts_with("#/")
            && let Some(target) = resolve_local_ref(schema, reference)
        {
            return target
                .get("properties")
                .and_then(Value::as_object)
                .is_some()
                || target
                    .get("patternProperties")
                    .and_then(Value::as_object)
                    .is_some();
        }
    }

    false
}

fn resolve_local_ref<'a>(schema: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    schema.pointer(pointer)
}

fn is_identity_field(name: &str) -> bool {
    matches!(name, "name" | "location" | "dateInstalled" | "manufacturer")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(device_type: &str) -> Vec<String> {
        electrical_fields(device_type)
            .into_iter()
            .map(|f| f.path)
            .collect()
    }

    #[test]
    fn electrical_schema_has_expected_types() {
        let types = electrical_types();

        assert!(types.iter().any(|t| t == "batteries"));
        assert!(types.iter().any(|t| t == "chargers"));
        assert!(types.iter().any(|t| t == "inverters"));
        assert!(types.iter().any(|t| t == "alternators"));
        assert!(types.iter().any(|t| t == "solar"));
        assert!(types.iter().any(|t| t == "ac"));
    }

    #[test]
    fn charger_inherits_common_fields() {
        let p = paths("chargers");

        assert!(p.iter().any(|x| x == "voltage"));
        assert!(p.iter().any(|x| x == "current"));
        assert!(p.iter().any(|x| x == "temperature"));
        assert!(p.iter().any(|x| x == "chargingMode"));
        assert!(p.iter().any(|x| x == "setpointVoltage"));
        assert!(p.iter().any(|x| x == "setpointCurrent"));
    }

    #[test]
    fn alternator_has_specific_and_inherited_fields() {
        let p = paths("alternators");

        assert!(p.iter().any(|x| x == "voltage"));
        assert!(p.iter().any(|x| x == "chargingMode"));
        assert!(p.iter().any(|x| x == "revolutions"));
        assert!(p.iter().any(|x| x == "fieldDrive"));
        assert!(p.iter().any(|x| x == "regulatorTemperature"));
    }

    #[test]
    fn inverter_exposes_nested_dc_and_ac_fields() {
        let p = paths("inverters");

        assert!(p.iter().any(|x| x == "dc.voltage"));
        assert!(p.iter().any(|x| x == "dc.current"));
        assert!(p.iter().any(|x| x == "ac.current"));
        assert!(p.iter().any(|x| x == "ac.frequency"));
        assert!(p.iter().any(|x| x == "inverterMode"));
    }
}
