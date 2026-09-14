use std::collections::{BTreeMap, HashSet};

use masterbus::FieldId;

use crate::mapping::{DeviceMapping, FieldMapping, Mapping, parse_field_key};
use crate::{seed, signalk};

/// Where a pre-filled path suggestion came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Existing,
    Suggested(seed::Tier),
    Blank,
}

/// Which part of a mapping is being edited.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Path,
    Truth(usize),
}

/// An in-progress edit of one field's Signal K path.
pub struct PathEditor {
    pub field: FieldId,
    pub field_name: String,
    pub unit: String,
    pub options: Vec<String>,
    pub buf: String,
    pub invert: bool,
    pub truth: BTreeMap<String, bool>,
    pub origin: Origin,
    pub stage: Stage,
}

/// What the editor tells the frontend about the path as typed.
pub enum Hint {
    Ok(String),
    Warn(String),
    Refuse(String),
}

impl PathEditor {
    pub fn plan(&self) -> Result<signalk::Plan, signalk::Refusal> {
        signalk::plan(self.buf.trim(), &self.unit, &self.options, &self.entry())
    }

    pub fn entry(&self) -> FieldMapping {
        FieldMapping {
            path: self.buf.trim().to_string(),
            invert: self.invert,
            truth: self.truth.clone(),
        }
    }

    pub fn hint(&self) -> Hint {
        match self.plan() {
            Err(signalk::Refusal::Truth { .. }) | Ok(_) if self.lossy_boolean() => {
                Hint::Warn(format!(
                    "{} labels → boolean loses info; use {} (string), or Enter for a truth table",
                    self.options.len(),
                    self.mode_leaf()
                ))
            }
            Err(e) => Hint::Refuse(e.to_string()),
            Ok(p) if !p.truth.is_empty() => Hint::Ok(format!(
                "boolean: {}",
                p.truth
                    .iter()
                    .map(|(k, v)| format!("{k}→{v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Ok(p) => match (p.unit, p.warning) {
                (_, Some(w)) => Hint::Warn(w),
                (None, None) => Hint::Ok("no unit: published as-is".into()),
                (Some(u), None) => Hint::Ok(format!("→ {u} ({})", p.conv.describe())),
            },
        }
    }

    pub fn lossy_boolean(&self) -> bool {
        self.options.len() > 2 && signalk::leaf_is_boolean(self.buf.trim())
    }

    pub fn mode_leaf(&self) -> &'static str {
        let p = self.buf.trim();
        if p.starts_with("electrical.inverters.") {
            "inverterMode"
        } else if p.starts_with("electrical.chargers.") || p.starts_with("electrical.solar.") {
            "chargingMode"
        } else {
            "a mode leaf"
        }
    }

    pub fn truth_complete(&self) -> bool {
        self.options.iter().all(|l| self.truth.contains_key(l))
    }
}

/// A device a mapping can be copied onto.
pub struct CopyTarget {
    pub serial: String,
    pub article: String,
    pub firmware: String,
    pub name: String,
    pub instance: String,
    pub have: HashSet<FieldId>,
}

/// Copy one device's field mappings onto every target, substituting each
/// target's own Signal K instance into the paths.
pub fn copy_to_targets(
    map: &mut Mapping,
    src: &DeviceMapping,
    targets: &[CopyTarget],
) -> (usize, usize) {
    let mut copied = 0usize;
    let mut skipped = 0usize;

    for t in targets {
        let entry = map.devices.entry(t.serial.clone()).or_default();

        entry.article = t.article.clone();
        entry.firmware = t.firmware.clone();
        entry.name = t.name.clone();

        if entry.instance.is_empty() {
            entry.instance = t.instance.clone();
        }

        let target_instance = entry.instance.clone();

        for (key, fm) in &src.fields {
            match parse_field_key(key) {
                Some(id) if t.have.contains(&id) => {
                    let from =
                        signalk::instance_of(&fm.path).unwrap_or_else(|| src.instance.clone());

                    let path = retarget(&fm.path, &from, &target_instance);

                    if path == fm.path {
                        skipped += 1;
                        continue;
                    }

                    entry.fields.insert(
                        key.to_string(),
                        FieldMapping {
                            path,
                            invert: fm.invert,
                            truth: fm.truth.clone(),
                        },
                    );

                    copied += 1;
                }
                _ => skipped += 1,
            }
        }
    }

    (copied, skipped)
}

/// Swap one instance segment for another inside a Signal K path.
pub fn retarget(path: &str, from: &str, to: &str) -> String {
    if from.is_empty() || from == to {
        return path.to_string();
    }

    path.split('.')
        .map(|seg| if seg == from { to } else { seg })
        .collect::<Vec<_>>()
        .join(".")
}
