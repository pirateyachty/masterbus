//! Signal K path mapping and unit conversion for MasterBus monitoring fields.
use masterbus::Value;

/// SI unit for a published Signal K path, keyed on its leaf segment. Signal K's
/// own metadata already carries units for the standard leaves (`voltage`,
/// `temperature`, …), but our non-standard nested leaves (`battery.temperature`,
/// `field.current`, `input.voltage`, `voltageSense`, …) are unknown to the
/// server, so it can't unit-convert them without a `meta` delta. We publish meta
/// for *every* known leaf (re-affirming the standard ones is harmless); `None`
/// leaves (`chargingMode`, `deviceMode`, `enabled`, `name`) carry no unit.
pub(super) fn sk_units(path: &str) -> Option<&'static str> {
    Some(match path.rsplit('.').next().unwrap_or("") {
        "stateOfCharge" => "ratio",
        "timeRemaining" => "s",
        "dischargeSinceFull" => "C",
        "temperature" => "K",
        "voltage" | "voltageSense" => "V",
        "current" | "currentLimit" => "A",
        "power" => "W",
        "frequency" | "revolutions" => "Hz",
        _ => return None,
    })
}

/// Signal K base path(s) for a device class — the node(s) that carry this
/// device's values, and thus its static `name` / `manufacturer` metadata. The
/// CombiMaster spans two categories. Keep this in sync with [`map_field`]'s
/// per-class path prefixes.
pub(super) fn sk_bases(class: &str, id: &str) -> Vec<String> {
    match class {
        "BAT" => vec![format!("electrical.batteries.{id}")],
        "CMR" => vec![
            format!("electrical.inverters.{id}"),
            format!("electrical.chargers.{id}"),
        ],
        "MAC" => vec![format!("electrical.chargers.{id}")],
        "APR" => vec![format!("electrical.alternators.{id}")],
        _ => vec![],
    }
}

/// Map a (device-class, field) pair to a Signal K path and SI value.
///
/// Returns `None` for fields without a mapping (they are simply not emitted).
/// Extend per device class; the matched names/units are exactly those the device
/// reports for its monitoring fields.
pub(super) fn map_field(
    class: &str,
    id: &str,
    _group: &str,
    name: &str,
    unit: &str,
    value: &Value,
) -> Option<(String, serde_json::Value)> {
    let celsius = match value {
        Value::Float(x) if x.is_finite() => Some(*x as f64),
        _ => None,
    };
    let float = celsius;
    let boolean = match value {
        Value::Boolean(b) => Some(*b),
        _ => None,
    };
    let seconds = match value {
        Value::Time(t) => Some(
            t.days as f64 * 86400.0 + t.hour as f64 * 3600.0 + t.min as f64 * 60.0 + t.sec as f64,
        ),
        _ => None,
    };
    // Selected label of a list/enum field (e.g. the charge-state name), lowercased.
    let list_label = value.label().map(|s| s.to_ascii_lowercase());
    let num = |v: f64| serde_json::Value::from(v);
    let text = |s: String| serde_json::Value::String(s);

    match class {
        // Battery monitors → electrical.batteries.<id>
        "BAT" => {
            let b = format!("electrical.batteries.{id}");
            match (name, unit) {
                ("State of charge", _) => {
                    float.map(|v| (format!("{b}.capacity.stateOfCharge"), num(v / 100.0)))
                }
                ("Battery", "V") => float.map(|v| (format!("{b}.voltage"), num(v))),
                ("Battery", "A") => float.map(|v| (format!("{b}.current"), num(v))),
                ("Battery", "\u{b0}C") => {
                    celsius.map(|c| (format!("{b}.temperature"), num(c + 273.15)))
                }
                ("Time remaining", _) => {
                    seconds.map(|s| (format!("{b}.capacity.timeRemaining"), num(s)))
                }
                ("Cap. consumed", _) => {
                    float.map(|ah| (format!("{b}.capacity.dischargeSinceFull"), num(ah * 3600.0)))
                }
                _ => None,
            }
        }
        // CombiMaster (inverter/charger) → electrical.inverters/chargers.<id>
        "CMR" => {
            let inv = format!("electrical.inverters.{id}");
            let chg = format!("electrical.chargers.{id}");
            match (name, unit) {
                ("Battery voltage", "V") => float.map(|v| (format!("{inv}.dc.voltage"), num(v))),
                ("Battery current", "A") => float.map(|v| (format!("{inv}.dc.current"), num(v))),
                ("Battery temp.", "\u{b0}C") => {
                    celsius.map(|c| (format!("{inv}.dc.temperature"), num(c + 273.15)))
                }
                ("Output voltage", "V") => float.map(|v| (format!("{inv}.ac.voltage"), num(v))),
                ("Output power", "W") => float.map(|v| (format!("{inv}.ac.power"), num(v))),
                ("Output frequency", "Hz") => {
                    float.map(|v| (format!("{inv}.ac.frequency"), num(v)))
                }
                ("Input voltage", "V") => float.map(|v| (format!("{chg}.acin.voltage"), num(v))),
                ("Input current", "A") => float.map(|v| (format!("{chg}.acin.current"), num(v))),
                ("Input frequency", "Hz") => {
                    float.map(|v| (format!("{chg}.acin.frequency"), num(v)))
                }
                ("AC IN limit", "A") => float.map(|v| (format!("{chg}.acin.currentLimit"), num(v))),
                ("Inverter", _) => {
                    boolean.map(|b| (format!("{inv}.enabled"), serde_json::Value::Bool(b)))
                }
                ("Charger", _) => {
                    boolean.map(|b| (format!("{chg}.enabled"), serde_json::Value::Bool(b)))
                }
                _ => None,
            }
        }
        // MAC — DC-DC battery charger (e.g. "MAC Plus 12/24"): a DC source on the
        // input steps up/down to charge the battery on the output. Canonical
        // charger `voltage`/`current`/`temperature` describe the output (battery)
        // side; the DC input side hangs off `.input.*`.
        //
        // Unlike the other classes, the MAC schema leaves the unit empty on some
        // monitoring fields (observed on MAC Plus 12/24: output voltage, output
        // current, input current, battery voltage sense), so a strict (name,
        // unit) match drops them silently. Those arms accept the unit *or* the
        // empty string. The rest keep the strict match on purpose: "Device" and
        // "Battery" are generic names that only °C tells apart, and wildcarding
        // them would publish any same-named float as a kelvin temperature.
        "MAC" => {
            let chg = format!("electrical.chargers.{id}");
            match (name, unit.trim()) {
                ("Output voltage", "V" | "") => float.map(|v| (format!("{chg}.voltage"), num(v))),
                ("Output current", "A" | "") => float.map(|v| (format!("{chg}.current"), num(v))),
                ("Input voltage", "V" | "") => {
                    float.map(|v| (format!("{chg}.input.voltage"), num(v)))
                }
                ("Input current", "A" | "") => {
                    float.map(|v| (format!("{chg}.input.current"), num(v)))
                }
                ("Bat. volt sense", "V" | "") => {
                    float.map(|v| (format!("{chg}.voltageSense"), num(v)))
                }
                ("Device", "\u{b0}C") => {
                    celsius.map(|c| (format!("{chg}.temperature"), num(c + 273.15)))
                }
                ("Battery", "\u{b0}C") => {
                    celsius.map(|c| (format!("{chg}.battery.temperature"), num(c + 273.15)))
                }
                // "Device state" (Standby/Charging/Fault/…) → deviceMode: the
                // device-level state, orthogonal to the charge stage below.
                ("Device state", _) => list_label.map(|s| (format!("{chg}.deviceMode"), text(s))),
                // "Charge state" (Bulk/Absorption/Float/…) → chargingMode.
                ("Charge state", _) => list_label.map(|s| (format!("{chg}.chargingMode"), text(s))),
                // "Standby" off = charger active.
                ("Standby", _) => {
                    boolean.map(|b| (format!("{chg}.enabled"), serde_json::Value::Bool(!b)))
                }
                _ => None,
            }
        }
        // APR — Alpha Pro alternator regulator ("APR Alternator"): a
        // mechanically-driven alternator plus an external shunt/battery monitor.
        // Canonical alternator `voltage`/`temperature`/`revolutions` describe the
        // alternator; the battery it charges (sensed both directly and via the
        // shunt) hangs off `.battery.*`, the engine drive off `.engine.*`.
        //
        // Note: "Battery voltage"/"Battery temp." occur in both the Battery and
        // Shunt groups (same name+unit, different field index); since `map_field`
        // sees no group they share one path and coalesce to the last sample of
        // the cycle — harmless, they read the same battery.
        "APR" => {
            let alt = format!("electrical.alternators.{id}");
            match (name, unit) {
                ("Alternator volt.", "V") => float.map(|v| (format!("{alt}.voltage"), num(v))),
                ("Sense voltage", "V") => float.map(|v| (format!("{alt}.voltageSense"), num(v))),
                ("Field current", "A") => float.map(|v| (format!("{alt}.field.current"), num(v))),
                ("Alternator temp.", "\u{b0}C") => {
                    celsius.map(|c| (format!("{alt}.temperature"), num(c + 273.15)))
                }
                // Shaft speeds → revolutions (Signal K wants Hz, i.e. rpm / 60).
                ("Alternator shaft", "rpm") => {
                    float.map(|r| (format!("{alt}.revolutions"), num(r / 60.0)))
                }
                ("Engine shaft", "rpm") => {
                    float.map(|r| (format!("{alt}.engine.revolutions"), num(r / 60.0)))
                }
                // "Charger state" (Off/Bulk/Absorption/Float/…) → chargingMode.
                ("Charger state", _) => {
                    list_label.map(|s| (format!("{alt}.chargingMode"), text(s)))
                }
                // Battery being charged (direct sense + external shunt).
                ("State of charge", "%") => {
                    float.map(|v| (format!("{alt}.battery.stateOfCharge"), num(v / 100.0)))
                }
                ("Battery voltage", "V") => {
                    float.map(|v| (format!("{alt}.battery.voltage"), num(v)))
                }
                ("Battery current", "A") => {
                    float.map(|v| (format!("{alt}.battery.current"), num(v)))
                }
                ("Battery temp.", "\u{b0}C") => {
                    celsius.map(|c| (format!("{alt}.battery.temperature"), num(c + 273.15)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `map_field` for a float monitoring value.
    fn f(class: &str, name: &str, unit: &str, v: f32) -> Option<(String, serde_json::Value)> {
        map_field(class, "1", "", name, unit, &Value::Float(v))
    }

    /// The path `map_field` produced, for assertions that only care about routing.
    fn path(r: Option<(String, serde_json::Value)>) -> Option<String> {
        r.map(|(p, _)| p)
    }

    /// The MAC schema reports an empty unit on several monitoring fields; they
    /// must still map, onto the same paths as when the unit is present.
    #[test]
    fn mac_maps_fields_with_a_missing_unit() {
        for (name, unit) in [
            ("Output voltage", "V"),
            ("Output current", "A"),
            ("Input current", "A"),
            ("Bat. volt sense", "V"),
        ] {
            assert_eq!(
                path(f("MAC", name, unit, 1.0)),
                path(f("MAC", name, "", 1.0)),
                "{name}"
            );
            assert!(
                path(f("MAC", name, "", 1.0)).is_some(),
                "{name} dropped with an empty unit"
            );
        }
    }

    /// The MAC output is the battery side, and lands on the canonical Signal K
    /// charger leaves — not on nested `output.*` / `device.*` ones the server
    /// has no metadata for.
    #[test]
    fn mac_publishes_canonical_charger_paths() {
        assert_eq!(
            path(f("MAC", "Output voltage", "", 27.4)).as_deref(),
            Some("electrical.chargers.1.voltage")
        );
        assert_eq!(
            path(f("MAC", "Output current", "", 42.0)).as_deref(),
            Some("electrical.chargers.1.current")
        );
        assert_eq!(
            path(f("MAC", "Device", "\u{b0}C", 20.0)).as_deref(),
            Some("electrical.chargers.1.temperature")
        );
    }

    /// Relaxing the unit match must not leak to the temperature fields: "Device"
    /// and "Battery" are generic names that only °C tells apart, so a same-named
    /// field in another unit must not be published as a kelvin temperature.
    #[test]
    fn mac_temperatures_still_require_degrees_celsius() {
        assert_eq!(f("MAC", "Device", "", 20.0), None);
        assert_eq!(f("MAC", "Battery", "V", 12.8), None);
        assert_eq!(f("MAC", "Battery", "A", 3.0), None);
    }

    /// Celsius → kelvin on the way out.
    #[test]
    fn mac_temperature_is_converted_to_kelvin() {
        let (_, v) = f("MAC", "Battery", "\u{b0}C", 25.0).unwrap();
        assert_eq!(v.as_f64().unwrap(), 298.15);
    }

    /// "Standby" is the inverse of Signal K's `enabled`.
    #[test]
    fn mac_standby_inverts_into_enabled() {
        let on = map_field("MAC", "1", "", "Standby", "", &Value::Boolean(false));
        assert_eq!(
            on,
            Some((
                "electrical.chargers.1.enabled".into(),
                serde_json::Value::Bool(true)
            ))
        );
        let off = map_field("MAC", "1", "", "Standby", "", &Value::Boolean(true));
        assert_eq!(
            off,
            Some((
                "electrical.chargers.1.enabled".into(),
                serde_json::Value::Bool(false)
            ))
        );
    }

    /// Enum fields publish their lowercased label.
    #[test]
    fn mac_enum_states_publish_their_label() {
        let list = |i: i32| Value::List {
            index: i,
            options: vec!["Off".into(), "Bulk".into()],
        };
        assert_eq!(
            map_field("MAC", "1", "", "Charge state", "", &list(1)),
            Some((
                "electrical.chargers.1.chargingMode".into(),
                serde_json::Value::String("bulk".into())
            ))
        );
        assert_eq!(
            path(map_field("MAC", "1", "", "Device state", "", &list(0))).as_deref(),
            Some("electrical.chargers.1.deviceMode")
        );
    }

    /// The other classes keep the strict (name, unit) match — BAT reports three
    /// different quantities all named "Battery", told apart only by their unit.
    #[test]
    fn other_classes_still_disambiguate_on_unit() {
        assert_eq!(
            path(f("BAT", "Battery", "V", 12.8)).as_deref(),
            Some("electrical.batteries.1.voltage")
        );
        assert_eq!(
            path(f("BAT", "Battery", "A", -5.0)).as_deref(),
            Some("electrical.batteries.1.current")
        );
        assert_eq!(
            path(f("BAT", "Battery", "\u{b0}C", 20.0)).as_deref(),
            Some("electrical.batteries.1.temperature")
        );
        assert_eq!(f("BAT", "Battery", "", 12.8), None);
    }

    /// Every leaf MAC publishes a number on must carry unit metadata, or Signal
    /// K cannot unit-convert it.
    #[test]
    fn mac_numeric_leaves_have_unit_metadata() {
        for (name, unit) in [
            ("Output voltage", ""),
            ("Output current", ""),
            ("Input voltage", "V"),
            ("Input current", ""),
            ("Bat. volt sense", ""),
            ("Device", "\u{b0}C"),
            ("Battery", "\u{b0}C"),
        ] {
            let p = path(f("MAC", name, unit, 1.0)).unwrap();
            assert!(sk_units(&p).is_some(), "{p} has no unit metadata");
        }
    }
}
