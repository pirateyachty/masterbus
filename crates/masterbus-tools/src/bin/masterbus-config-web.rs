//! Local web editor for masterbus-signalk-fields.toml.
//!
//! Serves a small dependency-light configuration UI and keeps the TOML file as
//! the single source of truth. Intended to run as a dedicated systemd service
//! on the MasterBus appliance.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_LISTEN: &str = "0.0.0.0:3010";
const DEFAULT_CONFIG: &str = "/etc/default/masterbus-signalk/masterbus-signalk-fields.toml";
const MAX_REQUEST_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PublicationConfig {
    #[serde(default)]
    devices: Vec<DeviceConfig>,
    #[serde(default)]
    fields: Vec<FieldConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceConfig {
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
    device: String,
    class: String,
    instance: String,
    group: String,
    index: String,
    field: String,
    unit: String,
    #[serde(default)]
    enabled: bool,
    suggested_path: String,
    path: String,
}

fn main() -> std::io::Result<()> {
    env_logger::init();

    let listen = env::var("MASTERBUS_CONFIG_WEB_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.into());
    let config_path = env::var("MASTERBUS_SIGNALK_FIELDS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG));

    let listener = TcpListener::bind(&listen)?;
    eprintln!(
        "masterbus-config-web: listening on http://{}; config={}",
        listen,
        config_path.display()
    );

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(err) = handle_connection(&mut stream, &config_path) {
                    eprintln!("masterbus-config-web: request error: {err}");
                    let _ = send_json(
                        &mut stream,
                        500,
                        &json!({"ok": false, "error": err.to_string()}),
                    );
                }
            }
            Err(err) => eprintln!("masterbus-config-web: accept error: {err}"),
        }
    }

    Ok(())
}

fn handle_connection(stream: &mut TcpStream, config_path: &Path) -> std::io::Result<()> {
    let req = read_request(stream)?;

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => send_response(
            stream,
            200,
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        ("GET", "/api/config") => {
            let cfg = load_config(config_path)?;
            send_json(stream, 200, &json!({"ok": true, "config": cfg}))
        }
        ("POST", "/api/config") => {
            let cfg: PublicationConfig = serde_json::from_slice(&req.body).map_err(io_other)?;
            validate_config(&cfg)?;
            save_config_atomic(config_path, &cfg)?;
            send_json(stream, 200, &json!({"ok": true}))
        }
        ("POST", "/api/restart") => {
            let status = Command::new("systemctl")
                .args(["restart", "masterbus-signalk"])
                .status()?;
            if status.success() {
                send_json(stream, 200, &json!({"ok": true}))
            } else {
                send_json(
                    stream,
                    500,
                    &json!({"ok": false, "error": "systemctl restart masterbus-signalk failed"}),
                )
            }
        }
        ("GET", "/api/status") => {
            let output = Command::new("systemctl")
                .args(["is-active", "masterbus-signalk"])
                .output()?;
            let active = String::from_utf8_lossy(&output.stdout).trim() == "active";
            send_json(stream, 200, &json!({"ok": true, "active": active}))
        }
        _ => send_json(stream, 404, &json!({"ok": false, "error": "not found"})),
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0_u8; 4096];
    let header_end;

    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Err(io_other("connection closed before request headers"));
        }
        buf.extend_from_slice(&tmp[..n]);

        if let Some(pos) = find_bytes(&buf, b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }

        if buf.len() > 64 * 1024 {
            return Err(io_other("request headers too large"));
        }
    }

    let header = std::str::from_utf8(&buf[..header_end]).map_err(io_other)?;
    let mut lines = header.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io_other("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| io_other("missing method"))?
        .to_string();
    let raw_path = parts.next().ok_or_else(|| io_other("missing path"))?;
    let path = raw_path.split('?').next().unwrap_or(raw_path).to_string();

    let mut content_length = 0_usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| io_other("invalid content-length"))?;
        }
    }

    if content_length > MAX_REQUEST_BODY {
        return Err(io_other("request body too large"));
    }

    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Err(io_other("connection closed before request body"));
        }
        buf.extend_from_slice(&tmp[..n]);
    }

    let body = buf[header_end..header_end + content_length].to_vec();
    Ok(HttpRequest { method, path, body })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn load_config(path: &Path) -> std::io::Result<PublicationConfig> {
    let text = fs::read_to_string(path)?;
    toml::from_str(&text).map_err(io_other)
}

fn validate_config(cfg: &PublicationConfig) -> std::io::Result<()> {
    let mut devices = HashSet::new();
    for d in &cfg.devices {
        if d.device.trim().is_empty() || d.class.trim().is_empty() || d.instance.trim().is_empty() {
            return Err(io_other(
                "device entries must have device, class, and instance",
            ));
        }
        if !devices.insert(d.device.clone()) {
            return Err(io_other(format!("duplicate device entry: {}", d.device)));
        }
    }

    let mut fields = HashSet::new();
    for f in &cfg.fields {
        if f.device.trim().is_empty()
            || f.index.trim().is_empty()
            || f.class.trim().is_empty()
            || f.instance.trim().is_empty()
        {
            return Err(io_other(
                "field entries are missing stable identity information",
            ));
        }

        let key = (f.device.clone(), f.index.clone());
        if !fields.insert(key) {
            return Err(io_other(format!(
                "duplicate field entry: device={} index={}",
                f.device, f.index
            )));
        }

        // Disabled/unmapped fields may legitimately have no Signal K path yet.
        // An enabled field must have a valid publication path.
        if f.enabled || !f.path.trim().is_empty() {
            validate_signalk_path(&f.path)?;
        }

        // suggested_path is mapper-owned guidance. Unknown/unmapped fields can
        // legitimately have an empty suggestion.
        if !f.suggested_path.trim().is_empty() {
            validate_signalk_path(&f.suggested_path)?;
        }
    }

    Ok(())
}

fn validate_signalk_path(path: &str) -> std::io::Result<()> {
    let p = path.trim();
    if p.is_empty() {
        return Err(io_other("Signal K paths may not be empty"));
    }
    if p.starts_with('.') || p.ends_with('.') || p.contains("..") {
        return Err(io_other(format!("invalid Signal K path: {p}")));
    }
    if !p
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(io_other(format!(
            "Signal K path contains unsupported characters: {p}"
        )));
    }
    Ok(())
}

fn save_config_atomic(path: &Path, cfg: &PublicationConfig) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io_other("configuration path has no parent"))?;
    fs::create_dir_all(parent)?;

    let rendered = render_config(cfg)?;
    // Parse our own output before touching the live file.
    let reparsed: PublicationConfig = toml::from_str(&rendered).map_err(io_other)?;
    validate_config(&reparsed)?;

    if path.exists() {
        let backup = path.with_extension("toml.backup");
        fs::copy(path, &backup)?;
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = parent.join(format!(".masterbus-signalk-fields.toml.{stamp}.tmp"));

    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(rendered.as_bytes())?;
        file.sync_all()?;
    }

    fs::rename(&tmp, path)?;
    Ok(())
}

fn render_config(cfg: &PublicationConfig) -> std::io::Result<String> {
    let mut out = String::from(
        "# masterbus-signalk field publication configuration.\n\
         # Managed by masterbus-config-web. Manual edits remain supported.\n\
         # Entries are keyed by MasterBus device address + field index.\n\n",
    );

    for d in &cfg.devices {
        out.push_str("[[devices]]\n");
        out.push_str(&format!("device = {}\n", toml_string(&d.device)));
        out.push_str(&format!("class = {}\n", toml_string(&d.class)));
        out.push_str(&format!("instance = {}\n", toml_string(&d.instance)));
        out.push_str(&format!("publish_name = {}\n", d.publish_name));
        out.push_str(&format!(
            "publish_manufacturer_name = {}\n",
            d.publish_manufacturer_name
        ));
        out.push_str(&format!("publish_model = {}\n\n", d.publish_model));
    }

    for f in &cfg.fields {
        out.push_str("[[fields]]\n");
        out.push_str(&format!("device = {}\n", toml_string(&f.device)));
        out.push_str(&format!("class = {}\n", toml_string(&f.class)));
        out.push_str(&format!("instance = {}\n", toml_string(&f.instance)));
        out.push_str(&format!("group = {}\n", toml_string(&f.group)));
        out.push_str(&format!("index = {}\n", toml_string(&f.index)));
        out.push_str(&format!("field = {}\n", toml_string(&f.field)));
        out.push_str(&format!("unit = {}\n", toml_string(&f.unit)));
        out.push_str(&format!("enabled = {}\n", f.enabled));
        out.push_str(&format!(
            "suggested_path = {}\n",
            toml_string(&f.suggested_path)
        ));
        out.push_str(&format!("path = {}\n\n", toml_string(&f.path)));
    }

    Ok(out)
}

fn toml_string(value: &str) -> String {
    // JSON string escaping is valid for TOML basic strings for the characters
    // used by this config and keeps quotes/backslashes safe.
    serde_json::to_string(value).unwrap()
}

fn send_json(
    stream: &mut TcpStream,
    status: u16,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io_other)?;
    send_response(stream, status, "application/json; charset=utf-8", &body)
}

fn send_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };

    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn io_other(err: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(err.to_string())
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>MasterBus Signal K Configuration</title>
<style>
:root{font-family:system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;color:#171717;background:#f4f4f5}
*{box-sizing:border-box} body{margin:0}.wrap{max-width:1500px;margin:auto;padding:22px}
header{display:flex;gap:18px;align-items:center;justify-content:space-between;margin-bottom:18px}
h1{font-size:24px;margin:0}.sub{color:#666;margin-top:4px;font-size:14px}
.toolbar{display:grid;grid-template-columns:minmax(220px,1fr) 220px 170px auto;gap:10px;margin-bottom:16px}
input,select,button{font:inherit} input[type=text],select{border:1px solid #ccc;border-radius:7px;padding:8px;background:white;width:100%}
button{border:1px solid #bbb;background:white;border-radius:7px;padding:8px 12px;cursor:pointer}
button.primary{background:#18181b;color:white;border-color:#18181b}button:disabled{opacity:.5;cursor:not-allowed}
.status{padding:9px 12px;border-radius:7px;background:#e4e4e7;font-size:14px}.status.good{background:#dcfce7}.status.bad{background:#fee2e2}
.device{background:white;border:1px solid #ddd;border-radius:10px;margin:12px 0;overflow:hidden}
.device-head{display:flex;justify-content:space-between;gap:16px;align-items:center;padding:13px 15px;background:#fafafa;cursor:pointer}
.device-title{font-weight:700}.muted{color:#737373;font-size:13px}.meta{display:flex;gap:15px;flex-wrap:wrap;font-size:13px}
.device-body{padding:0 14px 14px}.hidden{display:none}
table{width:100%;border-collapse:collapse;font-size:13px}th,td{text-align:left;padding:7px;border-bottom:1px solid #eee;vertical-align:middle}
th{position:sticky;top:0;background:white;color:#555}.path{min-width:430px}.suggested{color:#777;word-break:break-all;min-width:300px}
.custom{border-color:#f59e0b!important;background:#fffbeb!important}.reset{padding:5px 8px;font-size:12px}
.badpath{border-color:#dc2626!important;background:#fef2f2!important}.count{font-variant-numeric:tabular-nums}
@media(max-width:900px){.toolbar{grid-template-columns:1fr 1fr}.tablewrap{overflow:auto}.path{min-width:320px}}
</style>
</head>
<body>
<div class="wrap">
<header>
  <div><h1>MasterBus Signal K Configuration</h1><div class="sub">Edit field publication without hand-editing TOML.</div></div>
  <div id="serviceStatus" class="status">Checking service…</div>
</header>

<div class="toolbar">
  <input id="search" type="text" placeholder="Search device, group, field, or path…">
  <select id="deviceFilter"><option value="">All devices</option></select>
  <select id="enabledFilter"><option value="">All fields</option><option value="enabled">Enabled</option><option value="disabled">Disabled</option></select>
  <div style="display:flex;gap:8px"><button id="save">Save</button><button id="saveRestart" class="primary">Save & Restart</button></div>
</div>
<div id="message" class="status hidden"></div>
<div id="devices"></div>
</div>

<script>
let cfg=null, dirty=false;

const $=id=>document.getElementById(id);
function setMessage(text,kind=""){
  const el=$("message"); el.textContent=text; el.className="status"+(kind?" "+kind:"");
}
function markDirty(){dirty=true; document.title="* MasterBus Signal K Configuration";}
function pathValid(p,required=true){
  if(!p)return !required;
  return !p.startsWith(".") && !p.endsWith(".") && !p.includes("..") && /^[A-Za-z0-9._-]+$/.test(p);
}
function deviceLabel(d){return `${d.instance} · ${d.class} · ${d.device}`;}

async function load(){
  const r=await fetch("/api/config"); const j=await r.json();
  if(!j.ok) throw new Error(j.error||"Unable to load config");
  cfg=j.config;
  buildDeviceFilter();
  render();
  dirty=false; document.title="MasterBus Signal K Configuration";
}
function buildDeviceFilter(){
  const s=$("deviceFilter"); s.innerHTML='<option value="">All devices</option>';
  cfg.devices.forEach(d=>{const o=document.createElement("option");o.value=d.device;o.textContent=deviceLabel(d);s.appendChild(o)});
}

function render(){
  const root=$("devices"); root.innerHTML="";
  const q=$("search").value.toLowerCase().trim(), df=$("deviceFilter").value, ef=$("enabledFilter").value;

  cfg.devices.forEach(d=>{
    if(df && d.device!==df)return;
    const fields=cfg.fields.filter(f=>f.device===d.device).filter(f=>{
      if(ef==="enabled"&&!f.enabled)return false;
      if(ef==="disabled"&&f.enabled)return false;
      if(!q)return true;
      return [f.instance,f.group,f.field,f.unit,f.path,f.suggested_path,d.class,d.device].join(" ").toLowerCase().includes(q);
    });
    if(!fields.length && q)return;

    const card=document.createElement("section");card.className="device";
    const head=document.createElement("div");head.className="device-head";
    const left=document.createElement("div");
    const title=document.createElement("div"); title.className="device-title"; title.textContent=d.instance;
    const sub=document.createElement("div"); sub.className="muted"; sub.textContent=`${d.class} · ${d.device} · ${fields.filter(f=>f.enabled).length}/${fields.length} shown enabled`;
    left.append(title,sub);

    const meta=document.createElement("div");meta.className="meta";
    [["Name","publish_name"],["Manufacturer","publish_manufacturer_name"],["Model","publish_model"]].forEach(([label,key])=>{
      const l=document.createElement("label"), c=document.createElement("input");c.type="checkbox";c.checked=!!d[key];
      c.onchange=e=>{d[key]=e.target.checked;markDirty();};l.append(c,document.createTextNode(" "+label));meta.appendChild(l);
    });
    head.append(left,meta); card.appendChild(head);

    const body=document.createElement("div");body.className="device-body hidden";
    head.onclick=e=>{if(e.target.tagName!=="INPUT")body.classList.toggle("hidden")};

    const tw=document.createElement("div");tw.className="tablewrap";
    const table=document.createElement("table");
    table.innerHTML="<thead><tr><th>Enable</th><th>Group</th><th>Field</th><th>Unit</th><th>Suggested path</th><th>Published path</th><th></th></tr></thead>";
    const tbody=document.createElement("tbody");

    fields.forEach(f=>{
      const tr=document.createElement("tr");
      const ctd=document.createElement("td"), c=document.createElement("input");c.type="checkbox";c.checked=f.enabled;
      ctd.appendChild(c);

      const vals=[f.group,f.field,f.unit]; const cells=vals.map(v=>{const td=document.createElement("td");td.textContent=v;return td});
      const std=document.createElement("td");std.className="suggested";std.textContent=f.suggested_path || "—";
      const ptd=document.createElement("td"), inp=document.createElement("input");inp.type="text";inp.className="path";inp.value=f.path;
      const updateClass=()=>{
        inp.classList.toggle("custom",inp.value!==f.suggested_path);
        inp.classList.toggle("badpath",!pathValid(inp.value,f.enabled));
      };
      updateClass();
      c.onchange=e=>{f.enabled=e.target.checked;updateClass();markDirty();};
      inp.oninput=e=>{f.path=e.target.value;updateClass();markDirty();};ptd.appendChild(inp);
      const rtd=document.createElement("td"), reset=document.createElement("button");reset.className="reset";reset.textContent="Reset";
      reset.onclick=()=>{f.path=f.suggested_path;inp.value=f.path;updateClass();markDirty();};rtd.appendChild(reset);
      tr.append(ctd,...cells,std,ptd,rtd);tbody.appendChild(tr);
    });
    table.appendChild(tbody);tw.appendChild(table);body.appendChild(tw);card.appendChild(body);root.appendChild(card);
  });
}

async function save(restart){
  const invalid=cfg.fields.filter(f=>!pathValid(f.path,f.enabled));
  if(invalid.length){setMessage(`${invalid.length} enabled field(s) have an invalid Signal K path. Fix highlighted rows before saving.`,"bad");return;}
  $("save").disabled=$("saveRestart").disabled=true;
  try{
    let r=await fetch("/api/config",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(cfg)});
    let j=await r.json();if(!j.ok)throw new Error(j.error||"Save failed");
    dirty=false;document.title="MasterBus Signal K Configuration";
    if(restart){
      setMessage("Saved. Restarting masterbus-signalk…");
      r=await fetch("/api/restart",{method:"POST"});j=await r.json();if(!j.ok)throw new Error(j.error||"Restart failed");
      setMessage("Saved and masterbus-signalk restarted.","good");
      await status();
    }else setMessage("Configuration saved. Restart masterbus-signalk to apply changes.","good");
  }catch(e){setMessage(e.message,"bad")}
  finally{$("save").disabled=$("saveRestart").disabled=false}
}

async function status(){
  try{const r=await fetch("/api/status"),j=await r.json();const e=$("serviceStatus");e.textContent=j.active?"masterbus-signalk: active":"masterbus-signalk: inactive";e.className="status "+(j.active?"good":"bad")}catch{}
}

$("search").oninput=render;$("deviceFilter").onchange=render;$("enabledFilter").onchange=render;
$("save").onclick=()=>save(false);$("saveRestart").onclick=()=>save(true);
window.onbeforeunload=e=>{if(dirty){e.preventDefault();e.returnValue=""}};
load().catch(e=>setMessage(e.message,"bad"));status();setInterval(status,10000);
</script>
</body>
</html>"#;
