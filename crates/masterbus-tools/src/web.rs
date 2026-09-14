use std::{fs::File, path::Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct WebDevice {
    pub id: String,
    pub address: u32,
    pub article: String,
    pub serial: String,
    pub revision: String,
    pub name: String,
    pub firmware: String,
    pub status: String,
    pub groups: Vec<WebGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebGroup {
    pub id: u32,
    pub name: String,
    pub menu: String,
    pub fields: Vec<WebField>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebField {
    pub id: String,
    pub channel: String,
    pub index: u16,
    pub name: String,
    pub unit: String,
    pub value_text: String,
    pub options: Vec<String>,
    pub writeable: bool,
    pub eventable: bool,
}

#[derive(Debug, Deserialize)]
struct Dump {
    devices: Vec<DumpDevice>,
}

#[derive(Debug, Deserialize)]
struct DumpDevice {
    id: String,
    address: u32,
    article: String,
    serial: String,
    revision: String,
    name: String,
    firmware: String,
    status: String,
    groups: Vec<DumpGroup>,
}

#[derive(Debug, Deserialize)]
struct DumpGroup {
    id: u32,
    name: String,
    menu: String,
    fields: Vec<DumpField>,
}

#[derive(Debug, Deserialize)]
struct DumpField {
    id: String,
    channel: String,
    index: u16,
    name: String,
    unit: String,
    value_text: String,
    #[serde(default)]
    options: Vec<String>,
    writeable: bool,
    eventable: bool,
}

pub fn load_dump(path: &Path) -> Result<Vec<WebDevice>, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let dump: Dump = serde_json::from_reader(file)?;

    Ok(dump
        .devices
        .into_iter()
        .map(|d| WebDevice {
            id: d.id,
            address: d.address,
            article: d.article,
            serial: d.serial,
            revision: d.revision,
            name: d.name,
            firmware: d.firmware,
            status: d.status,
            groups: d
                .groups
                .into_iter()
                .map(|g| WebGroup {
                    id: g.id,
                    name: g.name,
                    menu: g.menu,
                    fields: g
                        .fields
                        .into_iter()
                        .map(|f| WebField {
                            id: f.id,
                            channel: f.channel,
                            index: f.index,
                            name: f.name,
                            unit: f.unit,
                            value_text: f.value_text,
                            options: f.options,
                            writeable: f.writeable,
                            eventable: f.eventable,
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect())
}
