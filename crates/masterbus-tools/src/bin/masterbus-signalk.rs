//! Signal K sidecar for Mastervolt MasterBus.
//!
//! Subscribes to the monitoring values of every device on the bus and serves
//! **Signal K deltas** as newline-delimited JSON over **TCP**. It listens on
//! `0.0.0.0:3009` by default; a Signal K server connects to it as a client
//! (data connection type: Signal K, over TCP).
//!
//! ```text
//! masterbus-signalk [listen-addr]
//! ```
//!
//! Transport (USB / SocketCAN), master role, and the schema cache directory
//! all come from the per-host config file (see `masterbus::FileConfig`); the
//! file is created on first run.
//!
//! The MasterBus-field → Signal K-path mapping (and unit conversion to SI) lives
//! in the `mapping` module; it currently covers batteries, the CombiMaster, the
//! MAC DC-DC charger, and the APR alternator regulator, and is easy to extend
//! per device class.
//!
//! # Which fields are published
//!
//! If the `MAPPING` environment variable points at a file, it gates output per
//! `<instance>.<menu>[.<group>]`. New devices are auto-added (menu-level = off;
//! the battery `cluster` group = on) and the file is rewritten; edit the
//! `true`/`false` flags while the service is stopped. Without `MAPPING`, every
//! mapped field is published.
//!
//! At startup every discovered monitoring field is also classified as `KNOWN`
//! (with its suggested Signal K path) or `UNMAPPED`. This inventory is diagnostic
//! only for now; it does not change the existing publication gating behavior.
//!
//! Besides live values, each published device also emits static `name` and
//! `manufacturer` (name + article/model) metadata once per client connection —
//! see [`static_meta_batch`].

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use masterbus::{Config, DeviceEvent, DeviceId, FieldId, MasterBus, Menu};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[path = "masterbus-signalk/mapping.rs"]
mod mapping;

use mapping::{map_field, sk_bases, sk_units, suggested_path};

/// Default TCP listen address.
const DEFAULT_LISTEN: &str = "0.0.0.0:3009";

/// How often each value is (re)emitted.
const RATE: Duration = Duration::from_millis(1000);

/// Default field-level publication configuration.
const DEFAULT_FIELDS_CONFIG: &str = "masterbus-signalk-fields.toml";

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let listen = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let fields_config = std::env::var_os("FIELDS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_FIELDS_CONFIG));

    let bus = match MasterBus::auto(Config::default()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("masterbus-signalk: connect failed: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(bus, &listen, &fields_config) {
        eprintln!("masterbus-signalk: {e}");
        std::process::exit(1);
    }
}

/// A discovered field and where it lives (used to build the mapping + metadata).
struct FieldRec {
    device: DeviceId,
    index: FieldId,
    class: String,
    instance: String,
    group: String,
    name: String,
    unit: String,
}

/// Per-device identity captured at startup, used to publish the static Signal K
/// `name` / `manufacturer` metadata for each device's node(s).
struct DeviceMetaRec {
    device: DeviceId,
    class: String,
    instance: String,
    /// Human-readable device name (as configured on the Mastervolt system).
    name: String,
    /// Article number → Signal K `manufacturer.model`.
    article: String,
}

/// Per-field metadata captured at startup so updates can be mapped cheaply.
struct FieldMeta {
    class: String,
    instance: String,
    group: String,
    name: String,
    unit: String,
    path: String,
}

/// Persistent field-level publication configuration.
///
/// `suggested_path` is refreshed from the built-in mapper on discovery. `path` is
/// the user's editable publication path. New entries are always disabled.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FieldsConfig {
    #[serde(default)]
    devices: Vec<DeviceConfig>,
    #[serde(default)]
    fields: Vec<FieldConfig>,
}

/// Per-device static metadata publication configuration.
///
/// These are deliberately separate from monitoring fields: support for name,
/// manufacturer name and model remains generic, while each item is explicitly
/// opt-in in the user's TOML. Metadata paths follow the effective user-edited
/// field base(s) for the device rather than forcing discovery-derived names.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceConfig {
    /// Stable MasterBus 24-bit device address (hex).
    device: String,
    class: String,
    instance: String,
    #[serde(default)]
    publish_name: bool,
    #[serde(default)]
    publish_manufacturer_name: bool,
    #[serde(default)]
    publish_model: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FieldConfig {
    /// Stable MasterBus 24-bit device address (hex).
    device: String,
    class: String,
    instance: String,
    group: String,
    index: String,
    field: String,
    unit: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    suggested_path: String,
    #[serde(default)]
    path: String,
}

fn device_key(device: DeviceId) -> String {
    format!("0x{device:06X}")
}

fn field_index_key(index: FieldId) -> String {
    index.to_string()
}

fn load_fields_config(path: &Path) -> std::io::Result<FieldsConfig> {
    if !path.exists() {
        return Ok(FieldsConfig::default());
    }
    let text = std::fs::read_to_string(path)?;
    toml::from_str(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid field config {}: {e}", path.display()),
        )
    })
}

fn save_fields_config(path: &Path, config: &FieldsConfig) -> std::io::Result<()> {
    let mut copy = config.clone();
    copy.devices.sort_by(|a, b| a.device.cmp(&b.device));
    copy.fields.sort_by(|a, b| {
        let a_index = a.index.parse::<u32>().unwrap_or(u32::MAX);
        let b_index = b.index.parse::<u32>().unwrap_or(u32::MAX);

        a.device
            .cmp(&b.device)
            .then_with(|| a_index.cmp(&b_index))
            .then_with(|| a.index.cmp(&b.index))
            .then_with(|| a.instance.cmp(&b.instance))
            .then_with(|| a.group.cmp(&b.group))
    });

    let mut out = String::from(
        "# masterbus-signalk field publication configuration.\n\
         # New fields are discovered automatically and default to enabled = false.\n\
         # `suggested_path` is maintained by the built-in mapper.\n\
         # Edit `path` to publish under a custom Signal K path.\n\
         # Entries are keyed by MasterBus device address + field index, so device-name\n\
         # changes do not discard enabled/path choices.\n\
         # Existing enabled/path choices are preserved when discovery runs again.\n\n",
    );
    out.push_str(
        &toml::to_string_pretty(&copy)
            .map_err(|e| std::io::Error::other(format!("could not serialize field config: {e}")))?,
    );
    std::fs::write(path, out)
}

/// Merge a discovered device into the persistent static-metadata configuration.
///
/// New devices default to publishing no static metadata. The stable MasterBus
/// address preserves the user's choices if the device is renamed later.
fn merge_device_config(config: &mut FieldsConfig, d: &DeviceMetaRec) -> bool {
    let device = device_key(d.device);
    if let Some(existing) = config.devices.iter_mut().find(|e| e.device == device) {
        let mut changed = false;
        if existing.class != d.class {
            existing.class = d.class.clone();
            changed = true;
        }
        if existing.instance != d.instance {
            existing.instance = d.instance.clone();
            changed = true;
        }
        return changed;
    }

    config.devices.push(DeviceConfig {
        device,
        class: d.class.clone(),
        instance: d.instance.clone(),
        publish_name: false,
        publish_manufacturer_name: false,
        publish_model: false,
    });
    true
}

fn configured_device(config: &FieldsConfig, device: DeviceId) -> Option<&DeviceConfig> {
    let key = device_key(device);
    config.devices.iter().find(|e| e.device == key)
}

/// Merge a discovered field into the persistent configuration.
///
/// If a mapper suggestion changes after a software upgrade and the user had left
/// `path` equal to the old suggestion, follow the new suggestion automatically.
/// A genuinely customized path is never overwritten.
fn merge_field_config(config: &mut FieldsConfig, f: &FieldRec) -> bool {
    let device = device_key(f.device);
    let index = field_index_key(f.index);
    let suggestion =
        suggested_path(&f.class, &f.instance, &f.group, &f.name, &f.unit).unwrap_or_default();

    if let Some(existing) = config
        .fields
        .iter_mut()
        .find(|e| e.device == device && e.index == index)
    {
        let old_suggestion = existing.suggested_path.clone();
        let path_was_default = existing.path.is_empty() || existing.path == old_suggestion;
        let mut changed = false;

        if existing.class != f.class {
            existing.class = f.class.clone();
            changed = true;
        }
        if existing.instance != f.instance {
            existing.instance = f.instance.clone();
            changed = true;
        }
        if existing.group != f.group {
            existing.group = f.group.clone();
            changed = true;
        }
        if existing.field != f.name {
            existing.field = f.name.clone();
            changed = true;
        }
        if existing.unit != f.unit {
            existing.unit = f.unit.clone();
            changed = true;
        }
        if existing.suggested_path != suggestion {
            existing.suggested_path = suggestion.clone();
            changed = true;
        }
        if path_was_default && existing.path != suggestion {
            existing.path = suggestion;
            changed = true;
        }
        return changed;
    }

    config.fields.push(FieldConfig {
        device,
        class: f.class.clone(),
        instance: f.instance.clone(),
        group: f.group.clone(),
        index,
        field: f.name.clone(),
        unit: f.unit.clone(),
        enabled: false,
        suggested_path: suggestion.clone(),
        path: suggestion,
    });
    true
}

fn configured_field<'a>(config: &'a FieldsConfig, f: &FieldRec) -> Option<&'a FieldConfig> {
    let device = device_key(f.device);
    let index = field_index_key(f.index);
    config
        .fields
        .iter()
        .find(|e| e.device == device && e.index == index)
}

/// Enumerate the Monitoring fields of a device into plain records.
/// Build the base Signal K/config instance name from the MasterBus-discovered
/// device label. This is intentionally generic: no device class or vessel-specific
/// names are hard-coded here.
fn discovered_instance(label: &str, full_name: &str, device: DeviceId) -> String {
    if !label.is_empty() {
        sanitize(label)
    } else if !full_name.is_empty() {
        sanitize(full_name)
    } else {
        device.to_string()
    }
}

fn discover_device_fields(dev: &masterbus::Device) -> (DeviceMetaRec, Vec<FieldRec>) {
    let name = dev.name().unwrap_or_default();
    let class = name.split_whitespace().next().unwrap_or("").to_string();
    let label = name
        .split_whitespace()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    let instance = discovered_instance(&label, &name, dev.id());

    let device_meta = DeviceMetaRec {
        device: dev.id(),
        class: class.clone(),
        instance: instance.clone(),
        name,
        article: dev.article_number().unwrap_or_default(),
    };

    let mut fields = Vec::new();
    if let Ok(groups) = dev.tab(Menu::Monitoring) {
        for group in groups {
            let gname = sanitize(&group.name().unwrap_or_default());
            for field in group.fields().unwrap_or_default() {
                fields.push(FieldRec {
                    device: dev.id(),
                    index: field.index(),
                    class: class.clone(),
                    instance: instance.clone(),
                    group: gname.clone(),
                    name: field.name().unwrap_or_default(),
                    unit: field.unit().unwrap_or_default(),
                });
            }
        }
    }
    (device_meta, fields)
}

/// Ensure instance names are unique within each MasterBus class.
///
/// Discovery remains authoritative. We only alter an instance when two devices of
/// the same class would otherwise publish under the exact same sanitized instance.
/// In that case the stable 24-bit MasterBus device address is appended.
fn disambiguate_instances(device_metas: &mut [DeviceMetaRec], fields: &mut [FieldRec]) {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();

    for d in device_metas.iter() {
        *counts
            .entry((d.class.clone(), d.instance.clone()))
            .or_default() += 1;
    }

    for d in device_metas.iter_mut() {
        let key = (d.class.clone(), d.instance.clone());
        if counts.get(&key).copied().unwrap_or(0) <= 1 {
            continue;
        }

        let old = d.instance.clone();
        let new = format!("{old}-{:06x}", d.device);
        d.instance = new.clone();

        for f in fields.iter_mut().filter(|f| f.device == d.device) {
            f.instance = new.clone();
        }
    }
}

fn report_field_inventory(fields: &[FieldRec]) {
    let mut known_fields = 0usize;
    let mut unmapped_fields = 0usize;
    for f in fields {
        match suggested_path(&f.class, &f.instance, &f.group, &f.name, &f.unit) {
            Some(path) => {
                known_fields += 1;
                eprintln!(
                    "KNOWN class={} instance={} group={} index={:?} field={:?} unit={:?} path={}",
                    f.class, f.instance, f.group, f.index, f.name, f.unit, path
                );
            }
            None => {
                unmapped_fields += 1;
                eprintln!(
                    "UNMAPPED class={} instance={} group={} index={:?} field={:?} unit={:?}",
                    f.class, f.instance, f.group, f.index, f.name, f.unit
                );
            }
        }
    }
    eprintln!(
        "masterbus-signalk: mapping inventory: {known_fields} known, {unmapped_fields} unmapped, {} total",
        fields.len()
    );
}

fn run(bus: MasterBus, listen: &str, fields_config_path: &Path) -> std::io::Result<()> {
    // TCP server: clients (e.g. a Signal K server) connect and receive the delta
    // stream. The listener thread appends new connections to the shared set.
    let listener = TcpListener::bind(listen)?;
    eprintln!(
        "masterbus-signalk: listening on {} (Signal K delta, ndjson)",
        listener.local_addr()?
    );
    let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
    // Static per-device metadata (name / manufacturer), rendered once discovery
    // completes. Replayed to every client the moment it connects so late joiners
    // still learn each device's identity without waiting for a value change.
    let static_batch: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let clients = clients.clone();
        let static_batch = static_batch.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                eprintln!(
                    "masterbus-signalk: client connected: {:?}",
                    stream.peer_addr().ok()
                );
                let sb = static_batch.lock().unwrap().clone();
                if !sb.is_empty() {
                    let _ = (&stream).write_all(&sb).and_then(|()| (&stream).flush());
                }
                clients.lock().unwrap().push(stream);
            }
        });
    }

    // Subscribe to device-presence events before the initial discovery snapshot.
    // Alive events that occur during startup remain queued and are filtered against
    // the initial known-device set once the streaming loop begins.
    let device_events = bus.device_events();

    // Allow MasterBus discovery to populate before taking the device snapshot.
    // Devices may announce at different times, especially on slower hardware.
    let discovery_start = std::time::Instant::now();
    let mut last_count = 0usize;

    loop {
        let count = bus.devices_all().len();

        if count != last_count {
            eprintln!("masterbus-signalk: discovered {count} device(s)");
            last_count = count;
        }

        if discovery_start.elapsed() >= Duration::from_secs(10) {
            eprintln!("masterbus-signalk: discovery complete with {count} device(s)");
            break;
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    // Enumerate every device that was present during the initial discovery window.
    let devices = bus.devices_all();
    let mut known_devices: HashSet<DeviceId> = devices.iter().map(|d| d.id()).collect();
    let mut fields: Vec<FieldRec> = Vec::new();
    let mut device_metas: Vec<DeviceMetaRec> = Vec::new();
    for dev in &devices {
        let (device_meta, mut device_fields) = discover_device_fields(dev);
        device_metas.push(device_meta);
        fields.append(&mut device_fields);
    }

    disambiguate_instances(&mut device_metas, &mut fields);
    report_field_inventory(&fields);

    // Merge discovery into the persistent field-level configuration. A malformed
    // config is fatal rather than being silently replaced and losing user choices.
    let mut field_config = load_fields_config(fields_config_path)?;
    let mut config_changed = !fields_config_path.exists();
    for d in &device_metas {
        config_changed |= merge_device_config(&mut field_config, d);
    }
    for f in &fields {
        config_changed |= merge_field_config(&mut field_config, f);
    }
    if config_changed {
        save_fields_config(fields_config_path, &field_config)?;
        eprintln!(
            "masterbus-signalk: wrote field config {}",
            fields_config_path.display()
        );
    }

    // Build subscriptions only for explicitly enabled fields. The mapper still
    // performs value conversion; the configured path replaces its suggested path.
    let mut meta: HashMap<(DeviceId, FieldId), FieldMeta> = HashMap::new();
    let mut per_device: HashMap<DeviceId, Vec<FieldId>> = HashMap::new();
    let mut units_by_path: HashMap<String, &'static str> = HashMap::new();

    for f in &fields {
        let Some(cfg) = configured_field(&field_config, f) else {
            continue;
        };
        if !cfg.enabled || cfg.path.trim().is_empty() {
            continue;
        }

        let Some(suggested) = suggested_path(&f.class, &f.instance, &f.group, &f.name, &f.unit)
        else {
            eprintln!(
                "masterbus-signalk: enabled field is UNMAPPED and cannot publish yet: class={} instance={} group={} index={:?} field={:?}",
                f.class, f.instance, f.group, f.index, f.name
            );
            continue;
        };

        let units = sk_units(&suggested);
        if let Some(u) = units {
            units_by_path.insert(cfg.path.clone(), u);
        }
        meta.insert(
            (f.device, f.index),
            FieldMeta {
                class: f.class.clone(),
                instance: f.instance.clone(),
                group: f.group.clone(),
                name: f.name.clone(),
                unit: f.unit.clone(),
                path: cfg.path.clone(),
            },
        );
        per_device.entry(f.device).or_default().push(f.index);
    }

    let mut published: HashSet<DeviceId> = per_device.keys().copied().collect();
    let mut subs = Vec::new();
    for (device, indices) in per_device {
        subs.push(bus.subscribe(device, indices, RATE, false));
    }

    // Render the static metadata batch and hand it to the accept thread (for
    // future clients) and to any client already connected during discovery.
    let sb = static_meta_batch(&device_metas, &published, &field_config);
    *static_batch.lock().unwrap() = sb.clone();
    if !sb.is_empty() {
        let mut cs = clients.lock().unwrap();
        cs.retain_mut(|c| c.write_all(&sb).and_then(|()| c.flush()).is_ok());
    }

    eprintln!(
        "masterbus-signalk: streaming {} of {} fields from {} initially discovered device(s) (field-config gated)",
        meta.len(),
        fields.len(),
        devices.len(),
    );

    // Paths whose unit `meta` has already been published. Meta is emitted inline
    // the first time a path is seen and also appended to `static_batch` so later
    // clients receive it on connect.
    let mut meta_sent: HashSet<String> = HashSet::new();

    loop {
        // Skip building deltas when nobody is listening (the channels are still
        // drained below so they don't grow unbounded).
        let have_clients = !clients.lock().unwrap().is_empty();
        let mut batch: Vec<u8> = Vec::new();
        let mut new_meta: Vec<serde_json::Value> = Vec::new();
        for sub in &subs {
            // Coalesce to the latest value per path this cycle: a field can be
            // updated many times between polls (the boat's real masters poll some
            // values rapidly, and we emit those too).
            let mut latest: HashMap<String, serde_json::Value> = HashMap::new();
            while let Some(u) = sub.try_recv() {
                if have_clients
                    && let Some(m) = meta.get(&(u.device, u.field))
                    && let Some((path, value)) =
                        map_field(&m.class, &m.instance, &m.group, &m.name, &m.unit, &u.value)
                {
                    let _ = path; // conversion path; publication path is user-configurable.
                    latest.insert(m.path.clone(), value);
                }
            }
            if !latest.is_empty() {
                // First sighting of a path → publish its unit metadata once.
                for path in latest.keys() {
                    if !meta_sent.contains(path)
                        && let Some(units) = units_by_path.get(path).copied()
                    {
                        new_meta.push(json!({ "path": path, "value": { "units": units } }));
                        meta_sent.insert(path.clone());
                    }
                }
                let values: Vec<_> = latest
                    .into_iter()
                    .map(|(path, value)| json!({ "path": path, "value": value }))
                    .collect();
                let delta = json!({
                    "updates": [{
                        "$source": "masterbus",
                        "timestamp": now_rfc3339(),
                        "values": values,
                    }]
                });
                batch.extend_from_slice(serde_json::to_string(&delta).unwrap().as_bytes());
                batch.push(b'\n');
            }
        }
        // Prepend any new unit metadata (so units land before/with the values)
        // and remember it for clients that connect later.
        if !new_meta.is_empty() {
            let delta = json!({
                "updates": [{ "$source": "masterbus", "timestamp": now_rfc3339(), "meta": new_meta }]
            });
            let mut line = serde_json::to_string(&delta).unwrap().into_bytes();
            line.push(b'\n');
            static_batch.lock().unwrap().extend_from_slice(&line);
            line.extend_from_slice(&batch);
            batch = line;
        }
        if !batch.is_empty() {
            let mut cs = clients.lock().unwrap();
            let before = cs.len();
            cs.retain_mut(|c| c.write_all(&batch).and_then(|()| c.flush()).is_ok());
            let dropped = before - cs.len();
            if dropped > 0 {
                eprintln!("masterbus-signalk: {dropped} client(s) disconnected");
            }
        }

        // Device presence is event-driven in the MasterBus runtime. Only an Alive
        // event for an ID that was absent from the initial discovery causes schema
        // enumeration here. Offline events deliberately do nothing: configuration
        // and subscriptions persist, so an initially known device can simply resume
        // producing updates when it broadcasts again.
        while let Ok(event) = device_events.try_recv() {
            let DeviceEvent::Alive(id) = event else {
                continue;
            };
            if known_devices.contains(&id) {
                continue;
            }

            known_devices.insert(id);
            let dev = bus.device(id);
            let (mut device_meta, mut device_fields) = discover_device_fields(&dev);

            if device_metas
                .iter()
                .any(|d| d.class == device_meta.class && d.instance == device_meta.instance)
            {
                let new_instance = format!("{}-{:06x}", device_meta.instance, id);
                device_meta.instance = new_instance.clone();
                for f in &mut device_fields {
                    f.instance = new_instance.clone();
                }
            }

            eprintln!(
                "masterbus-signalk: new device discovered during runtime: {:?} name={:?}",
                id, device_meta.name
            );
            report_field_inventory(&device_fields);

            let mut config_changed = merge_device_config(&mut field_config, &device_meta);
            for f in &device_fields {
                config_changed |= merge_field_config(&mut field_config, f);
            }

            if config_changed {
                if let Err(e) = save_fields_config(fields_config_path, &field_config) {
                    eprintln!(
                        "masterbus-signalk: could not update field config {}: {e}",
                        fields_config_path.display()
                    );
                } else {
                    eprintln!(
                        "masterbus-signalk: updated field config {} with newly discovered device fields",
                        fields_config_path.display()
                    );
                }
            }

            // Brand-new entries are disabled. If this device had appeared on an
            // earlier run, however, matching persisted entries may already be enabled;
            // subscribe to those immediately.
            let mut indices = Vec::new();
            for f in &device_fields {
                let Some(cfg) = configured_field(&field_config, f) else {
                    continue;
                };
                if !cfg.enabled || cfg.path.trim().is_empty() {
                    continue;
                }

                let Some(suggested) =
                    suggested_path(&f.class, &f.instance, &f.group, &f.name, &f.unit)
                else {
                    eprintln!(
                        "masterbus-signalk: enabled field is UNMAPPED and cannot publish yet: class={} instance={} group={} index={:?} field={:?}",
                        f.class, f.instance, f.group, f.index, f.name
                    );
                    continue;
                };

                if let Some(u) = sk_units(&suggested) {
                    units_by_path.insert(cfg.path.clone(), u);
                }
                meta.insert(
                    (f.device, f.index),
                    FieldMeta {
                        class: f.class.clone(),
                        instance: f.instance.clone(),
                        group: f.group.clone(),
                        name: f.name.clone(),
                        unit: f.unit.clone(),
                        path: cfg.path.clone(),
                    },
                );
                indices.push(f.index);
            }

            if !indices.is_empty() {
                subs.push(bus.subscribe(id, indices, RATE, false));
                published.insert(id);
            }

            device_metas.push(device_meta);
            fields.extend(device_fields);

            // Refresh the persistent static metadata snapshot. Only devices with
            // at least one enabled/published field are included.
            let sb = static_meta_batch(&device_metas, &published, &field_config);
            *static_batch.lock().unwrap() = sb.clone();
            if !sb.is_empty() {
                let mut cs = clients.lock().unwrap();
                cs.retain_mut(|c| c.write_all(&sb).and_then(|()| c.flush()).is_ok());
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Resolve the effective Signal K base path(s) for a device from enabled fields.
///
/// For each built-in suggested base, take the suffix of a field's suggested path
/// and apply that same suffix to the user's configured path. This makes a user
/// rename such as `fwd-charger` -> `fwdAC` carry through to static metadata too,
/// without hard-coding vessel-specific names in Rust.
fn effective_bases_for_device(d: &DeviceMetaRec, config: &FieldsConfig) -> HashSet<String> {
    let device = device_key(d.device);
    let mut bases = HashSet::new();

    for f in config
        .fields
        .iter()
        .filter(|f| f.device == device && f.enabled && !f.path.trim().is_empty())
    {
        for suggested_base in sk_bases(&d.class, &d.instance) {
            let suffix = if f.suggested_path == suggested_base {
                ""
            } else if let Some(suffix) = f
                .suggested_path
                .strip_prefix(&(suggested_base.clone() + "."))
            {
                // Restore the dot so we can remove exactly the mapped leaf suffix
                // from the custom publication path.
                // `suffix_owned` lives only in this branch, so handle it inline.
                let suffix = format!(".{suffix}");
                if let Some(custom_base) = f.path.strip_suffix(&suffix) {
                    if !custom_base.is_empty() {
                        bases.insert(custom_base.to_string());
                    }
                }
                continue;
            } else {
                continue;
            };

            if suffix.is_empty() {
                bases.insert(f.path.clone());
            }
        }
    }

    bases
}

/// Build the one-shot Signal K static metadata batch.
///
/// Support for device name, manufacturer name and model remains in the generic
/// sidecar, but every item is explicitly opt-in through `[[devices]]` in the
/// TOML. Metadata is emitted at the effective user-configured base path(s).
fn static_meta_batch(
    devs: &[DeviceMetaRec],
    published: &HashSet<DeviceId>,
    config: &FieldsConfig,
) -> Vec<u8> {
    let mut batch = Vec::new();
    for d in devs {
        if !published.contains(&d.device) {
            continue;
        }
        let Some(dc) = configured_device(config, d.device) else {
            continue;
        };
        if !dc.publish_name && !dc.publish_manufacturer_name && !dc.publish_model {
            continue;
        }

        let mut values = Vec::new();
        for base in effective_bases_for_device(d, config) {
            if dc.publish_name && !d.name.is_empty() {
                values.push(json!({ "path": format!("{base}.name"), "value": d.name }));
            }
            if dc.publish_manufacturer_name {
                values.push(json!({
                    "path": format!("{base}.manufacturer.name"),
                    "value": "Mastervolt"
                }));
            }
            if dc.publish_model && !d.article.is_empty() {
                values.push(json!({
                    "path": format!("{base}.manufacturer.model"),
                    "value": d.article
                }));
            }
        }
        if values.is_empty() {
            continue;
        }
        let delta = json!({
            "updates": [{ "$source": "masterbus", "timestamp": now_rfc3339(), "values": values }]
        });
        batch.extend_from_slice(serde_json::to_string(&delta).unwrap().as_bytes());
        batch.push(b'\n');
    }
    batch
}

/// Lowercase and keep only Signal K path-segment-safe characters in an instance
/// id (lowercase reads more idiomatically in Signal K paths).
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "0".into()
    } else {
        cleaned
    }
}

/// Current UTC time as an ISO-8601 / RFC-3339 string (no date dependency).
fn now_rfc3339() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let (y, mo, day) = civil_from_days((secs / 86400) as i64);
    format!("{y:04}-{mo:02}-{day:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

#[cfg(test)]
mod field_config_tests {
    use super::*;

    fn sample_field(instance: &str) -> FieldRec {
        FieldRec {
            device: 0x123456,
            index: 7,
            class: "CMR".into(),
            instance: instance.into(),
            group: "monitoring".into(),
            name: "Battery voltage".into(),
            unit: "V".into(),
        }
    }

    #[test]
    fn discovered_instance_uses_device_label_generically() {
        assert_eq!(discovered_instance("COMBI", "MCU COMBI", 0x186442), "combi");
        assert_eq!(
            discovered_instance("COMBI 2", "MCU COMBI 2", 0x19B442),
            "combi-2"
        );
        assert_eq!(
            discovered_instance("House Inverter", "MCU House Inverter", 0x123456),
            "house-inverter"
        );
    }

    #[test]
    fn duplicate_instances_get_stable_device_id_suffixes() {
        let mut metas = vec![
            DeviceMetaRec {
                device: 0x111111,
                class: "MCU".into(),
                instance: "combi".into(),
                name: "MCU COMBI".into(),
                article: String::new(),
            },
            DeviceMetaRec {
                device: 0x222222,
                class: "MCU".into(),
                instance: "combi".into(),
                name: "MCU COMBI".into(),
                article: String::new(),
            },
        ];
        let mut fields = vec![
            FieldRec {
                device: 0x111111,
                index: 1,
                class: "MCU".into(),
                instance: "combi".into(),
                group: "general".into(),
                name: "Device state".into(),
                unit: String::new(),
            },
            FieldRec {
                device: 0x222222,
                index: 1,
                class: "MCU".into(),
                instance: "combi".into(),
                group: "general".into(),
                name: "Device state".into(),
                unit: String::new(),
            },
        ];

        disambiguate_instances(&mut metas, &mut fields);

        assert_eq!(metas[0].instance, "combi-111111");
        assert_eq!(metas[1].instance, "combi-222222");
        assert_eq!(fields[0].instance, "combi-111111");
        assert_eq!(fields[1].instance, "combi-222222");
    }

    #[test]
    fn new_discovered_field_defaults_disabled_with_suggested_path() {
        let mut cfg = FieldsConfig::default();
        let field = sample_field("combi");

        assert!(merge_field_config(&mut cfg, &field));
        assert_eq!(cfg.fields.len(), 1);

        let entry = &cfg.fields[0];
        assert_eq!(entry.device, "0x123456");
        assert_eq!(entry.index, "7");
        assert!(!entry.enabled);
        assert_eq!(
            entry.suggested_path,
            "electrical.inverters.combi.dc.voltage"
        );
        assert_eq!(entry.path, entry.suggested_path);
    }

    #[test]
    fn customized_path_survives_device_name_change() {
        let mut cfg = FieldsConfig::default();
        let original = sample_field("old-name");
        merge_field_config(&mut cfg, &original);

        cfg.fields[0].enabled = true;
        cfg.fields[0].path = "electrical.inverters.house.dc.voltage".into();

        let renamed = sample_field("new-name");
        assert!(merge_field_config(&mut cfg, &renamed));

        let entry = &cfg.fields[0];
        assert!(entry.enabled);
        assert_eq!(entry.instance, "new-name");
        assert_eq!(entry.path, "electrical.inverters.house.dc.voltage");
        assert_eq!(
            entry.suggested_path,
            "electrical.inverters.new-name.dc.voltage"
        );
    }

    #[test]
    fn untouched_default_path_follows_new_suggestion() {
        let mut cfg = FieldsConfig::default();
        merge_field_config(&mut cfg, &sample_field("old-name"));

        assert!(merge_field_config(&mut cfg, &sample_field("new-name")));
        assert_eq!(
            cfg.fields[0].path,
            "electrical.inverters.new-name.dc.voltage"
        );
    }

    #[test]
    fn new_device_metadata_defaults_off() {
        let mut cfg = FieldsConfig::default();
        let d = DeviceMetaRec {
            device: 0x123456,
            class: "CHG".into(),
            instance: "fwd-charger".into(),
            name: "CHG Fwd Charger".into(),
            article: "44320405".into(),
        };
        assert!(merge_device_config(&mut cfg, &d));
        let dc = configured_device(&cfg, d.device).unwrap();
        assert!(!dc.publish_name);
        assert!(!dc.publish_manufacturer_name);
        assert!(!dc.publish_model);
    }

    #[test]
    fn custom_field_base_is_used_for_device_metadata() {
        let d = DeviceMetaRec {
            device: 0x123456,
            class: "CHG".into(),
            instance: "fwd-charger".into(),
            name: "CHG Fwd Charger".into(),
            article: "44320405".into(),
        };
        let mut cfg = FieldsConfig::default();
        merge_device_config(&mut cfg, &d);
        cfg.devices[0].publish_name = true;
        cfg.devices[0].publish_model = true;
        cfg.devices[0].publish_manufacturer_name = false;
        cfg.fields.push(FieldConfig {
            device: device_key(d.device),
            class: "CHG".into(),
            instance: "fwd-charger".into(),
            group: "monitoring".into(),
            index: "2".into(),
            field: "Output 1".into(),
            unit: "V".into(),
            enabled: true,
            suggested_path: "electrical.chargers.fwd-charger.voltage".into(),
            path: "electrical.chargers.fwdAC.voltage".into(),
        });

        let bases = effective_bases_for_device(&d, &cfg);
        assert!(bases.contains("electrical.chargers.fwdAC"));
        assert!(!bases.contains("electrical.chargers.fwd-charger"));

        let published = HashSet::from([d.device]);
        let batch = String::from_utf8(static_meta_batch(&[d], &published, &cfg)).unwrap();
        assert!(batch.contains("electrical.chargers.fwdAC.name"));
        assert!(batch.contains("electrical.chargers.fwdAC.manufacturer.model"));
        assert!(!batch.contains("manufacturer.name"));
        assert!(!batch.contains("electrical.chargers.fwd-charger"));
    }
}
