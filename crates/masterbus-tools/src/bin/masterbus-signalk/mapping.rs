//! Signal K path mapping and unit conversion for MasterBus monitoring fields.
//!
//! This module deliberately keeps vessel-specific naming out of the Rust code.
//! Device instances come from MasterBus discovery, while group-aware rules map
//! known Mastervolt classes to sensible Signal K paths. Device-specific/user
//! overrides can be layered on top later without changing the transport code.

use masterbus::Value;

/// SI unit for a published Signal K path, keyed on its leaf segment.
///
/// Standard Signal K leaves already have server-side metadata, but the MasterBus
/// vendor namespace contains additional leaves, so the sidecar emits unit metadata
/// for any numeric leaf it knows about.
pub(super) fn sk_units(path: &str) -> Option<&'static str> {
    Some(match path.rsplit('.').next().unwrap_or("") {
        "stateOfCharge" | "currentLimitRatio" | "backlight" => "ratio",
        "timeRemaining" | "remaining" | "standbyTime" | "pageDuration" => "s",
        "dischargeSinceFull" => "C",
        "temperature" => "K",
        "voltage" | "voltageSense" | "senseVoltage" | "altVoltage" => "V",
        "current" | "currentLimit" | "fieldCurrent" | "mainsFuse" => "A",
        "power" | "solarInput" => "W",
        "frequency" | "revolutions" => "Hz",
        _ => return None,
    })
}

/// Signal K base path(s) for a device class.
///
/// These are used for static device metadata (`name`, manufacturer/model). A
/// device may publish both to a standard Signal K category and to the MasterBus
/// vendor namespace.
pub(super) fn sk_bases(class: &str, id: &str) -> Vec<String> {
    match class {
        "BAT" | "MSH" => vec![
            format!("electrical.batteries.{id}"),
            format!("electrical.masterbus.{id}"),
        ],
        "CMR" | "MCU" => vec![
            format!("electrical.inverters.{id}"),
            format!("electrical.chargers.{id}"),
            format!("electrical.masterbus.{id}"),
        ],
        "MAC" | "INT" | "CHG" => vec![
            format!("electrical.chargers.{id}"),
            format!("electrical.masterbus.{id}"),
        ],
        "APR" => vec![
            format!("electrical.alternators.{id}"),
            format!("electrical.masterbus.{id}"),
        ],
        "DIS" => vec![format!("electrical.masterbus.{id}")],
        _ => vec![],
    }
}

/// Convert a user/device-provided label into a stable Signal K path segment.
///
/// This is intentionally generic: EasyView switch names are user-defined, so a
/// switch called "Fwd DC/DC" becomes `fwd-dc-dc` without any vessel-specific
/// lookup table.
fn field_slug(s: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;

    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            pending_dash = false;
        } else {
            pending_dash = !out.is_empty();
        }
    }

    out.trim_end_matches('-').to_string()
}

fn text_value(value: &Value) -> Option<serde_json::Value> {
    match value {
        Value::Boolean(b) => Some(serde_json::Value::Bool(*b)),
        Value::Float(x) if x.is_finite() => Some(serde_json::Value::from(*x as f64)),
        Value::List { index, .. } | Value::Eventable { index, .. } => {
            if let Some(label) = value.label() {
                Some(serde_json::Value::String(label.to_string()))
            } else {
                Some(serde_json::Value::from(*index))
            }
        }
        Value::Text { text, .. } => Some(serde_json::Value::String(text.clone())),
        Value::DeviceRef { index, .. } => Some(serde_json::Value::from(*index)),
        Value::Date(d) if d.year > 0 && d.mon > 0 && d.day > 0 => Some(serde_json::Value::String(
            format!("{:04}-{:02}-{:02}", d.year, d.mon, d.day),
        )),
        Value::Time(t) if t.sec >= 0 && t.min >= 0 && t.hour >= 0 => {
            let prefix = if t.days > 0 {
                format!("{}d ", t.days)
            } else {
                String::new()
            };
            Some(serde_json::Value::String(format!(
                "{prefix}{:02}:{:02}:{:02}",
                t.hour, t.min, t.sec
            )))
        }
        _ => None,
    }
}

fn float_value(value: &Value) -> Option<f64> {
    match value {
        Value::Float(x) if x.is_finite() => Some(*x as f64),
        _ => None,
    }
}

fn seconds_value(value: &Value) -> Option<f64> {
    match value {
        Value::Time(t) if t.sec >= 0 && t.min >= 0 && t.hour >= 0 => Some(
            t.days as f64 * 86400.0 + t.hour as f64 * 3600.0 + t.min as f64 * 60.0 + t.sec as f64,
        ),
        _ => None,
    }
}

fn label_value(value: &Value, lowercase: bool) -> Option<serde_json::Value> {
    value.label().map(|s| {
        serde_json::Value::String(if lowercase {
            s.to_ascii_lowercase()
        } else {
            s.to_string()
        })
    })
}

fn bool_or_label(value: &Value) -> Option<serde_json::Value> {
    match value {
        Value::Boolean(b) => Some(serde_json::Value::Bool(*b)),
        _ => label_value(value, false).or_else(|| text_value(value)),
    }
}

fn celsius_to_kelvin(value: &Value) -> Option<serde_json::Value> {
    float_value(value).map(|c| serde_json::Value::from(c + 273.15))
}

fn percent_to_ratio(value: &Value) -> Option<serde_json::Value> {
    float_value(value).map(|v| serde_json::Value::from(v / 100.0))
}

fn amp_hours_to_coulombs(value: &Value) -> Option<serde_json::Value> {
    float_value(value).map(|ah| serde_json::Value::from(ah * 3600.0))
}

fn rpm_to_hz(value: &Value) -> Option<serde_json::Value> {
    float_value(value).map(|rpm| serde_json::Value::from(rpm / 60.0))
}

fn numeric(value: &Value) -> Option<serde_json::Value> {
    float_value(value).map(serde_json::Value::from)
}

/// Map a discovered MasterBus field to a Signal K path and value.
///
/// Matching is group-aware because several Mastervolt devices expose identical
/// field labels in different monitoring groups. Returns `None` for fields that
/// are invalid or genuinely not useful to publish.
pub(super) fn map_field(
    class: &str,
    id: &str,
    group: &str,
    name: &str,
    unit: &str,
    value: &Value,
) -> Option<(String, serde_json::Value)> {
    let unit = unit.trim();

    let mapped = match class {
        // ---------------------------------------------------------------------
        // BAT — Mastervolt lithium batteries / battery monitor.
        // ---------------------------------------------------------------------
        "BAT" => {
            match group {
                // Individual battery data uses canonical battery paths.
                "battery" => {
                    let b = format!("electrical.batteries.{id}");
                    match (name, unit) {
                        ("State of charge", "%") => percent_to_ratio(value)
                            .map(|v| (format!("{b}.capacity.stateOfCharge"), v)),
                        ("Time remaining", _) => seconds_value(value)
                            .map(serde_json::Value::from)
                            .map(|v| (format!("{b}.capacity.timeRemaining"), v)),
                        ("Voltage", "V") => numeric(value).map(|v| (format!("{b}.voltage"), v)),
                        ("Current", "A") => numeric(value).map(|v| (format!("{b}.current"), v)),
                        ("Temperature", "°C") => {
                            celsius_to_kelvin(value).map(|v| (format!("{b}.temperature"), v))
                        }
                        _ => None,
                    }
                }

                // A clustered bank is a separate aggregate view; keep it distinct
                // from the individual battery until a vessel override promotes it.
                "cluster" => {
                    let b = format!("electrical.masterbus.{id}.cluster");
                    match (name, unit) {
                        ("State of charge", "%") => {
                            percent_to_ratio(value).map(|v| (format!("{b}.stateOfCharge"), v))
                        }
                        ("Time remaining", _) => seconds_value(value)
                            .map(serde_json::Value::from)
                            .map(|v| (format!("{b}.timeRemaining"), v)),
                        ("Voltage", "V") => numeric(value).map(|v| (format!("{b}.voltage"), v)),
                        ("Current", "A") => numeric(value).map(|v| (format!("{b}.current"), v)),
                        ("Temperature", "°C") => {
                            celsius_to_kelvin(value).map(|v| (format!("{b}.temperature"), v))
                        }
                        _ => None,
                    }
                }

                "relay" => {
                    let slug = field_slug(name);
                    if slug.is_empty() {
                        None
                    } else {
                        text_value(value)
                            .map(|v| (format!("electrical.masterbus.{id}.relay.{slug}"), v))
                    }
                }
                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // MSH — MasterShunt-style battery monitor.
        // ---------------------------------------------------------------------
        "MSH" if group == "general" => {
            let b = format!("electrical.batteries.{id}");
            match (name, unit) {
                ("State of Charge", "%") => {
                    percent_to_ratio(value).map(|v| (format!("{b}.capacity.stateOfCharge"), v))
                }
                ("Remaining", _) => seconds_value(value)
                    .map(serde_json::Value::from)
                    .map(|v| (format!("{b}.capacity.timeRemaining"), v)),
                ("Cap. consumed", "Ah") => amp_hours_to_coulombs(value)
                    .map(|v| (format!("{b}.capacity.dischargeSinceFull"), v)),
                ("Battery", "V") => numeric(value).map(|v| (format!("{b}.voltage"), v)),
                ("Battery", "A") => numeric(value).map(|v| (format!("{b}.current"), v)),
                ("Battery", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{b}.temperature"), v))
                }
                ("Time", _) => {
                    text_value(value).map(|v| (format!("electrical.masterbus.{id}.clock.time"), v))
                }
                ("Date", _) => {
                    text_value(value).map(|v| (format!("electrical.masterbus.{id}.clock.date"), v))
                }
                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // CMR — existing CombiMaster support retained.
        // ---------------------------------------------------------------------
        "CMR" => {
            let inv = format!("electrical.inverters.{id}");
            let chg = format!("electrical.chargers.{id}");
            match (name, unit) {
                ("Battery voltage", "V") => {
                    numeric(value).map(|v| (format!("{inv}.dc.voltage"), v))
                }
                ("Battery current", "A") => {
                    numeric(value).map(|v| (format!("{inv}.dc.current"), v))
                }
                ("Battery temp.", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{inv}.dc.temperature"), v))
                }
                ("Output voltage", "V") => numeric(value).map(|v| (format!("{inv}.ac.voltage"), v)),
                ("Output power", "W") => numeric(value).map(|v| (format!("{inv}.ac.power"), v)),
                ("Output frequency", "Hz") => {
                    numeric(value).map(|v| (format!("{inv}.ac.frequency"), v))
                }
                ("Input voltage", "V") => {
                    numeric(value).map(|v| (format!("{chg}.acin.voltage"), v))
                }
                ("Input current", "A") => {
                    numeric(value).map(|v| (format!("{chg}.acin.current"), v))
                }
                ("Input frequency", "Hz") => {
                    numeric(value).map(|v| (format!("{chg}.acin.frequency"), v))
                }
                ("AC IN limit", "A") => {
                    numeric(value).map(|v| (format!("{chg}.acin.currentLimit"), v))
                }
                ("Inverter", _) => bool_or_label(value).map(|v| (format!("{inv}.enabled"), v)),
                ("Charger", _) => bool_or_label(value).map(|v| (format!("{chg}.enabled"), v)),
                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // MCU — Mass Combi Ultra.
        //
        // The MCU is fundamentally an inverter/charger, so normal electrical
        // values are published under the standard inverter/charger namespaces.
        // The MasterBus namespace is reserved for device-specific state that
        // does not have a clean standard Signal K home.
        // ---------------------------------------------------------------------
        "MCU" => {
            let inv = format!("electrical.inverters.{id}");
            let chg = format!("electrical.chargers.{id}");
            let mb = format!("electrical.masterbus.{id}");

            match (group, name, unit) {
                // -------------------------------------------------------------
                // General device state
                // -------------------------------------------------------------
                ("general", "Device state", _) => label_value(value, false)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{mb}.deviceState"), v)),

                ("general", "Mains fuse", "A") => {
                    numeric(value).map(|v| (format!("{inv}.mainsFuse"), v))
                }

                ("general", "Inverter", _) => {
                    bool_or_label(value).map(|v| (format!("{inv}.state"), v))
                }

                ("general", "User mode", _) => {
                    bool_or_label(value).map(|v| (format!("{mb}.userMode"), v))
                }

                ("general", "AC in state", _) => {
                    bool_or_label(value).map(|v| (format!("{inv}.acInState"), v))
                }

                ("general", "AC out state", _) => {
                    bool_or_label(value).map(|v| (format!("{inv}.acOutState"), v))
                }

                ("general", "Main charger", _) => {
                    bool_or_label(value).map(|v| (format!("{chg}.state"), v))
                }

                ("general", "Sec. charger", _) => {
                    bool_or_label(value).map(|v| (format!("{chg}.secondary.state"), v))
                }

                ("general", "Solar charger", _) => {
                    bool_or_label(value).map(|v| (format!("{chg}.solar.state"), v))
                }

                // -------------------------------------------------------------
                // Main battery / DC side
                // -------------------------------------------------------------
                ("battery--dc-", "Main charger", _) => {
                    bool_or_label(value).map(|v| (format!("{chg}.state"), v))
                }

                ("battery--dc-", "Main battery", "V") => {
                    numeric(value).map(|v| (format!("{inv}.dc.voltage"), v))
                }

                ("battery--dc-", "Main battery", "A") => {
                    numeric(value).map(|v| (format!("{inv}.dc.current"), v))
                }

                ("battery--dc-", "Main battery", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{inv}.dc.temperature"), v))
                }

                // -------------------------------------------------------------
                // Secondary charger
                // -------------------------------------------------------------
                ("sec--charger", "Sec. charger", _) => {
                    bool_or_label(value).map(|v| (format!("{chg}.secondary.state"), v))
                }

                ("sec--charger", "Sec. battery", "V") => {
                    numeric(value).map(|v| (format!("{chg}.secondary.voltage"), v))
                }

                ("sec--charger", "Sec. battery", "A") => {
                    numeric(value).map(|v| (format!("{chg}.secondary.current"), v))
                }

                // -------------------------------------------------------------
                // Cluster AC input
                //
                // These are aggregate values for the MCU inverter cluster rather
                // than measurements belonging only to this individual unit.
                // -------------------------------------------------------------
                ("cluster-ac-in", "Mains", "V") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acInMains.voltage"), v))
                }

                ("cluster-ac-in", "Mains", "A") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acInMains.current"), v))
                }

                ("cluster-ac-in", "Mains", "W") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acInMains.power"), v))
                }

                ("cluster-ac-in", "Generator", "V") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acInGenerator.voltage"), v))
                }

                ("cluster-ac-in", "Generator", "A") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acInGenerator.current"), v))
                }

                ("cluster-ac-in", "Generator", "W") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acInGenerator.power"), v))
                }

                // -------------------------------------------------------------
                // Cluster AC output
                // -------------------------------------------------------------
                ("cluster-ac-out", "AC Output 1", "V") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acOut1.voltage"), v))
                }

                ("cluster-ac-out", "AC Output 1", "W") => {
                    numeric(value).map(|v| (format!("{inv}.cluster.acOut1.power"), v))
                }

                // -------------------------------------------------------------
                // This MCU's individual AC inputs
                // -------------------------------------------------------------
                ("ac-inputs", "Mains", "V") => {
                    numeric(value).map(|v| (format!("{inv}.acInputs.mains.voltage"), v))
                }

                ("ac-inputs", "Mains", "A") => {
                    numeric(value).map(|v| (format!("{inv}.acInputs.mains.current"), v))
                }

                ("ac-inputs", "Mains", "W") => {
                    numeric(value).map(|v| (format!("{inv}.acInputs.mains.power"), v))
                }

                ("ac-inputs", "Generator", "V") => {
                    numeric(value).map(|v| (format!("{inv}.acInputs.generator.voltage"), v))
                }

                ("ac-inputs", "Generator", "A") => {
                    numeric(value).map(|v| (format!("{inv}.acInputs.generator.current"), v))
                }

                ("ac-inputs", "Generator", "W") => {
                    numeric(value).map(|v| (format!("{inv}.acInputs.generator.power"), v))
                }

                // -------------------------------------------------------------
                // This MCU's individual AC outputs
                // -------------------------------------------------------------
                ("ac-outputs", "AC Output 1", "V") => {
                    numeric(value).map(|v| (format!("{inv}.acOutputs.output1.voltage"), v))
                }

                ("ac-outputs", "AC Output 1", "A") => {
                    numeric(value).map(|v| (format!("{inv}.acOutputs.output1.current"), v))
                }

                ("ac-outputs", "AC Output 1", "W") => {
                    numeric(value).map(|v| (format!("{inv}.acOutputs.output1.power"), v))
                }

                ("ac-outputs", "AC Output 2", "V") => {
                    numeric(value).map(|v| (format!("{inv}.acOutputs.output2.voltage"), v))
                }

                ("ac-outputs", "AC Output 2", "A") => {
                    numeric(value).map(|v| (format!("{inv}.acOutputs.output2.current"), v))
                }

                ("ac-outputs", "AC Output 2", "W") => {
                    numeric(value).map(|v| (format!("{inv}.acOutputs.output2.power"), v))
                }

                // -------------------------------------------------------------
                // Integrated solar charger
                // -------------------------------------------------------------
                ("solar-input", "State", _) => {
                    bool_or_label(value).map(|v| (format!("{chg}.solar.state"), v))
                }

                ("solar-input", "Solar Input", "W") => {
                    numeric(value).map(|v| (format!("{chg}.solar.power"), v))
                }

                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // MAC — Mass DC/DC charger.
        // Supports both Kees' observed schema and the group-aware schema from the
        // field inventory.
        // ---------------------------------------------------------------------
        "MAC" => {
            let c = format!("electrical.chargers.{id}");
            match (group, name, unit) {
                ("status", "Device state", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{c}.deviceMode"), v)),
                ("status", "Charge state", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{c}.chargingMode"), v)),
                ("status", "On/Standby", _) => {
                    bool_or_label(value).map(|v| (format!("{c}.state"), v))
                }

                ("dc-48v", "Thrust 48v [V]", "V") => {
                    numeric(value).map(|v| (format!("{c}.dc48.voltage"), v))
                }
                ("dc-48v", "Thrust 48v [A]", "A") => {
                    numeric(value).map(|v| (format!("{c}.dc48.current"), v))
                }
                ("dc-48v", "Bat. volt sense", "V") => {
                    numeric(value).map(|v| (format!("{c}.voltageSense"), v))
                }

                ("dc-24v", "House 24v [V]", "V") => {
                    numeric(value).map(|v| (format!("{c}.dc24.voltage"), v))
                }
                ("dc-24v", "House 24v [A]", "A") => {
                    numeric(value).map(|v| (format!("{c}.dc24.current"), v))
                }

                ("remote", "Remote input", _) => {
                    bool_or_label(value).map(|v| (format!("{c}.remoteInput"), v))
                }
                ("temperature", "Device", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{c}.temperature"), v))
                }
                ("temperature", "Battery", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{c}.battery.temperature"), v))
                }

                // Compatibility with the older generic MAC schema.
                (_, "Output voltage", "V" | "") => {
                    numeric(value).map(|v| (format!("{c}.voltage"), v))
                }
                (_, "Output current", "A" | "") => {
                    numeric(value).map(|v| (format!("{c}.current"), v))
                }
                (_, "Input voltage", "V" | "") => {
                    numeric(value).map(|v| (format!("{c}.input.voltage"), v))
                }
                (_, "Input current", "A" | "") => {
                    numeric(value).map(|v| (format!("{c}.input.current"), v))
                }
                (_, "Bat. volt sense", "V" | "") => {
                    numeric(value).map(|v| (format!("{c}.voltageSense"), v))
                }
                (_, "Device", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{c}.temperature"), v))
                }
                (_, "Battery", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{c}.battery.temperature"), v))
                }
                (_, "Device state", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{c}.deviceMode"), v)),
                (_, "Charge state", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{c}.chargingMode"), v)),
                (_, "Standby", _) => match value {
                    Value::Boolean(b) => {
                        Some((format!("{c}.enabled"), serde_json::Value::Bool(!*b)))
                    }
                    _ => None,
                },

                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // INT — Mastervolt interface / Mac-Magic DC/DC device.
        // ---------------------------------------------------------------------
        "INT" if group == "mac-magic" => {
            let c = format!("electrical.chargers.{id}");
            match (name, unit) {
                ("Mode", _) => bool_or_label(value).map(|v| (format!("{c}.deviceMode"), v)),
                ("Mac/Magic On", _) => bool_or_label(value).map(|v| (format!("{c}.state"), v)),
                ("DC Input", "V") => numeric(value).map(|v| (format!("{c}.input.voltage"), v)),
                ("DC Output", "V") => numeric(value).map(|v| (format!("{c}.voltage"), v)),
                ("DC Output", "A") => numeric(value).map(|v| (format!("{c}.current"), v)),
                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // CHG — Mastervolt AC chargers. Different models expose different menu
        // schemas, so group is essential here.
        // ---------------------------------------------------------------------
        "CHG" => {
            let c = format!("electrical.chargers.{id}");
            match (group, name, unit) {
                ("general", "Device state", _) => {
                    bool_or_label(value).map(|v| (format!("{c}.deviceMode"), v))
                }
                ("general", "On/Stand-by", _) => {
                    bool_or_label(value).map(|v| (format!("{c}.state"), v))
                }
                ("general", "Max. current", "%") => {
                    percent_to_ratio(value).map(|v| (format!("{c}.currentLimitRatio"), v))
                }
                ("general", "State", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{c}.chargingMode"), v)),
                ("general", "Charger temp", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{c}.temperature"), v))
                }

                ("output", "Battery name", _) => {
                    text_value(value).map(|v| (format!("{c}.battery.name"), v))
                }
                ("output", "Battery voltage", "V") => {
                    numeric(value).map(|v| (format!("{c}.voltage"), v))
                }
                ("output", "Battery current", "A") => {
                    numeric(value).map(|v| (format!("{c}.current"), v))
                }
                ("output", "Bat. temperature", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{c}.battery.temperature"), v))
                }

                ("monitoring", "State", _) => {
                    bool_or_label(value).map(|v| (format!("{c}.deviceMode"), v))
                }
                ("monitoring", "State of charger", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{c}.chargingMode"), v)),
                ("monitoring", "Charger", _) => {
                    bool_or_label(value).map(|v| (format!("{c}.state"), v))
                }
                ("monitoring", "Set max current", "A") => {
                    numeric(value).map(|v| (format!("{c}.currentLimit"), v))
                }
                ("monitoring", "Output 1", "V") => {
                    numeric(value).map(|v| (format!("{c}.voltage"), v))
                }
                ("monitoring", "Output 1", "A") => {
                    numeric(value).map(|v| (format!("{c}.current"), v))
                }
                ("monitoring", "Output 2", "V") => {
                    numeric(value).map(|v| (format!("{c}.output2.voltage"), v))
                }
                ("monitoring", "Output 2", "A") => {
                    numeric(value).map(|v| (format!("{c}.output2.current"), v))
                }
                ("monitoring", "Output 3", "V") => {
                    numeric(value).map(|v| (format!("{c}.output3.voltage"), v))
                }
                ("monitoring", "Output 3", "A") => {
                    numeric(value).map(|v| (format!("{c}.output3.current"), v))
                }
                ("monitoring", "Battery", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{c}.battery.temperature"), v))
                }

                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // APR — Alpha Pro alternator regulator.
        // Group-aware handling preserves the regulator's battery and shunt views
        // separately instead of coalescing same-named fields.
        // ---------------------------------------------------------------------
        "APR" => {
            let a = format!("electrical.alternators.{id}");
            match (group, name, unit) {
                ("general", "Device state", _) => {
                    bool_or_label(value).map(|v| (format!("{a}.deviceState"), v))
                }
                ("general", "Charger state", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{a}.chargingMode"), v)),

                ("battery", "Battery voltage", "V") => {
                    numeric(value).map(|v| (format!("{a}.battery.voltage"), v))
                }
                ("battery", "Battery temp.", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{a}.battery.temperature"), v))
                }

                ("alternator", "Alternator volt.", "V") => {
                    numeric(value).map(|v| (format!("{a}.voltage"), v))
                }
                ("alternator", "Sense voltage", "V") => {
                    numeric(value).map(|v| (format!("{a}.voltageSense"), v))
                }
                ("alternator", "Field current", "A") => {
                    numeric(value).map(|v| (format!("{a}.field.current"), v))
                }
                ("alternator", "Alternator temp.", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{a}.temperature"), v))
                }
                ("alternator", "Alternator shaft", "rpm") => {
                    rpm_to_hz(value).map(|v| (format!("{a}.revolutions"), v))
                }
                ("alternator", "Engine shaft", "rpm") => {
                    rpm_to_hz(value).map(|v| (format!("{a}.engine.revolutions"), v))
                }

                ("shunt", "State", _) => {
                    bool_or_label(value).map(|v| (format!("{a}.shunt.state"), v))
                }
                ("shunt", "State of charge", "%") => {
                    percent_to_ratio(value).map(|v| (format!("{a}.shunt.stateOfCharge"), v))
                }
                ("shunt", "Battery voltage", "V") => {
                    numeric(value).map(|v| (format!("{a}.shunt.voltage"), v))
                }
                ("shunt", "Battery current", "A") => {
                    numeric(value).map(|v| (format!("{a}.shunt.current"), v))
                }
                ("shunt", "Battery temp.", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{a}.shunt.temperature"), v))
                }

                // Compatibility fallback for APR schemas without useful group names.
                (_, "Alternator volt.", "V") => numeric(value).map(|v| (format!("{a}.voltage"), v)),
                (_, "Sense voltage", "V") => {
                    numeric(value).map(|v| (format!("{a}.voltageSense"), v))
                }
                (_, "Field current", "A") => {
                    numeric(value).map(|v| (format!("{a}.field.current"), v))
                }
                (_, "Alternator temp.", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{a}.temperature"), v))
                }
                (_, "Alternator shaft", "rpm") => {
                    rpm_to_hz(value).map(|v| (format!("{a}.revolutions"), v))
                }
                (_, "Engine shaft", "rpm") => {
                    rpm_to_hz(value).map(|v| (format!("{a}.engine.revolutions"), v))
                }
                (_, "Charger state", _) => label_value(value, true)
                    .or_else(|| text_value(value))
                    .map(|v| (format!("{a}.chargingMode"), v)),
                (_, "State of charge", "%") => {
                    percent_to_ratio(value).map(|v| (format!("{a}.battery.stateOfCharge"), v))
                }
                (_, "Battery voltage", "V") => {
                    numeric(value).map(|v| (format!("{a}.battery.voltage"), v))
                }
                (_, "Battery current", "A") => {
                    numeric(value).map(|v| (format!("{a}.battery.current"), v))
                }
                (_, "Battery temp.", "°C") => {
                    celsius_to_kelvin(value).map(|v| (format!("{a}.battery.temperature"), v))
                }

                _ => None,
            }
        }

        // ---------------------------------------------------------------------
        // DIS — EasyView display.
        //
        // Switch labels are user-created on the display, so they are intentionally
        // wildcarded: every non-empty discovered field in the `switches` group is
        // published using a sanitized form of its actual label.
        // ---------------------------------------------------------------------
        "DIS" => {
            let b = format!("electrical.masterbus.{id}");
            match group {
                "switches" => {
                    let slug = field_slug(name);
                    if slug.is_empty() {
                        None
                    } else {
                        text_value(value).map(|v| (format!("{b}.switches.{slug}.state"), v))
                    }
                }

                "general" => match name {
                    "Language" => text_value(value).map(|v| (format!("{b}.display.language"), v)),
                    "Alarm sound" => {
                        bool_or_label(value).map(|v| (format!("{b}.display.alarmSound"), v))
                    }
                    _ => None,
                },

                "power-save" => match (name, unit) {
                    ("Standby time", _) => seconds_value(value)
                        .map(serde_json::Value::from)
                        .or_else(|| text_value(value))
                        .map(|v| (format!("{b}.display.powerSave.standbyTime"), v)),
                    ("Auto off", _) => {
                        bool_or_label(value).map(|v| (format!("{b}.display.powerSave.autoOff"), v))
                    }
                    ("Backlight", "%") => percent_to_ratio(value)
                        .map(|v| (format!("{b}.display.powerSave.backlight"), v)),
                    _ => None,
                },

                "widgets" => match name {
                    "Page duration" => seconds_value(value)
                        .map(serde_json::Value::from)
                        .or_else(|| text_value(value))
                        .map(|v| (format!("{b}.display.widgets.pageDuration"), v)),
                    "Slideshow" => {
                        bool_or_label(value).map(|v| (format!("{b}.display.widgets.slideshow"), v))
                    }
                    _ => None,
                },

                _ => None,
            }
        }

        _ => None,
    };

    mapped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(
        class: &str,
        group: &str,
        name: &str,
        unit: &str,
        v: f32,
    ) -> Option<(String, serde_json::Value)> {
        map_field(class, "1", group, name, unit, &Value::Float(v))
    }

    fn path(r: Option<(String, serde_json::Value)>) -> Option<String> {
        r.map(|(p, _)| p)
    }

    #[test]
    fn easyview_switch_uses_discovered_label() {
        let on = Value::Boolean(true);
        assert_eq!(
            map_field("DIS", "easyview-5", "switches", "Fwd DC/DC", "", &on),
            Some((
                "electrical.masterbus.easyview-5.switches.fwd-dc-dc.state".into(),
                serde_json::Value::Bool(true)
            ))
        );
    }

    #[test]
    fn easyview_empty_switch_is_ignored() {
        assert_eq!(
            map_field(
                "DIS",
                "easyview-5",
                "switches",
                "",
                "",
                &Value::Boolean(true)
            ),
            None
        );
    }

    #[test]
    fn battery_group_and_cluster_do_not_collide() {
        assert_eq!(
            path(f("BAT", "battery", "Voltage", "V", 26.4)).as_deref(),
            Some("electrical.batteries.1.voltage")
        );
        assert_eq!(
            path(f("BAT", "cluster", "Voltage", "V", 26.4)).as_deref(),
            Some("electrical.masterbus.1.cluster.voltage")
        );
    }

    #[test]
    fn mcu_same_label_is_disambiguated_by_group() {
        assert_eq!(
            path(f("MCU", "cluster-ac-in", "Mains", "V", 230.0)).as_deref(),
            Some("electrical.inverters.1.cluster.acInMains.voltage")
        );
        assert_eq!(
            path(f("MCU", "ac-inputs", "Mains", "V", 230.0)).as_deref(),
            Some("electrical.inverters.1.acInputs.mains.voltage")
        );
    }

    #[test]
    fn apr_shunt_and_battery_views_remain_distinct() {
        assert_eq!(
            path(f("APR", "battery", "Battery voltage", "V", 27.0)).as_deref(),
            Some("electrical.alternators.1.battery.voltage")
        );
        assert_eq!(
            path(f("APR", "shunt", "Battery voltage", "V", 27.0)).as_deref(),
            Some("electrical.alternators.1.shunt.voltage")
        );
    }

    #[test]
    fn mac_missing_unit_compatibility_is_retained() {
        assert_eq!(
            path(f("MAC", "", "Output voltage", "", 27.4)).as_deref(),
            Some("electrical.chargers.1.voltage")
        );
        assert_eq!(
            path(f("MAC", "", "Output current", "", 42.0)).as_deref(),
            Some("electrical.chargers.1.current")
        );
    }

    #[test]
    fn temperature_is_converted_to_kelvin() {
        let (_, v) = f("MAC", "temperature", "Battery", "°C", 25.0).unwrap();
        assert!((v.as_f64().unwrap() - 298.15).abs() < 1e-9);
    }

    #[test]
    fn rpm_is_converted_to_hz() {
        let (_, v) = f("APR", "alternator", "Alternator shaft", "rpm", 1800.0).unwrap();
        assert_eq!(v.as_f64().unwrap(), 30.0);
    }

    #[test]
    fn known_numeric_paths_have_unit_metadata() {
        for p in [
            "electrical.batteries.house.voltage",
            "electrical.masterbus.combi.acInputs.mains.current",
            "electrical.masterbus.combi.solarInput.power",
            "electrical.alternators.alt-110a.field.current",
            "electrical.chargers.fwd-charger.currentLimit",
        ] {
            assert!(sk_units(p).is_some(), "{p} has no unit metadata");
        }
    }
}
