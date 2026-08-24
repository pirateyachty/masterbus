# For end users

Let's say you have a MasterBus network (at least one device, probably more)
and you want to integrate your MasterBus data — battery State Of Charge,
current draw, inverter status — into some other software. But you're NOT
a programmer, and you thought Rust was what happens when iron oxidizes.

How can this project help? It's not *so* hard.

## What you need

A computer that can reach the bus. Two options, both covered in
[HARDWARE.md](HARDWARE.md):

- A **Mastervolt USB Interface** (article 77030200, ~€200) — works on
  Linux, macOS, and Windows. Plug-and-play, no fiddling.
- A **Linux machine with a cheap CAN adapter** — a CANable USB stick
  (~€25), a PiCAN HAT on a Raspberry Pi, or any SocketCAN-capable port.
  Bring up `can0` at 250 kbit/s and you're set.

Then download the latest binary release from
<https://github.com/keesverruijt/masterbus/releases> *(coming soon —
until then, build from source: `cargo build --release` produces
`target/release/masterbus-tui` and `target/release/masterbus-signalk`).*

## First steps: explore the bus

Run `masterbus-tui` to see what's on your bus. It's a terminal
application that lists every device, lets you drill into its menus
(Summary, Monitoring, Configuration, Service, Settings), and shows live
values. Writable fields can be edited inline.

    masterbus-tui

The first run creates a config file with the detected transport (USB 
link if present, otherwise the lone CAN interface) or fails with
an error if there are multiple CAN devices. 
Edit that file to switch transports or enable bus-master
mode; see the **Configuration** section in the project README.

You'll see a left pane with all alive devices and a right pane with
that device's tabs. `Tab` / `Shift+Tab` switch tabs, arrow keys move
between fields, `Enter` opens an editor on writable fields, `l` opens
the access-level (login) modal. Higher access levels unlock more
fields — for many configuration changes you need at least Installer.

The first time you connect to a device it takes a few seconds to
discover its schema; afterwards everything is cached on disk and the
TUI feels instant.

## Reading data continuously

For pulling data off the bus in a long-running stream, use
`masterbus-signalk`. It discovers the devices and monitoring fields on the bus
and serves Signal K deltas (newline-delimited JSON) on a TCP socket.

    masterbus-signalk 0.0.0.0:3009

On first discovery it creates `masterbus-signalk-fields.toml` in the current
working directory. **Discovered fields are not published automatically.** Each
new field starts with `enabled = false`; edit the TOML and explicitly enable the
fields you want.

Each field also has both a mapper-provided `suggested_path` and a user-editable
`path`, so you can keep the suggested Signal K naming or use names that make more
sense for your installation:

```toml
[[fields]]
device = "0x123456"
class = "CHG"
instance = "fwd-charger"
group = "monitoring"
index = "2"
field = "Output 1"
unit = "V"
enabled = true
suggested_path = "electrical.chargers.fwd-charger.voltage"
path = "electrical.chargers.fwdAC.voltage"
```

Static device metadata is also opt-in per device. `name`,
`manufacturer.name`, and `manufacturer.model` can be enabled independently in
the same TOML. If you customize a field's base path, enabled metadata follows
that customized base too.

Existing choices are preserved when discovery runs again. New devices or fields
that appear later are appended disabled, so a device that was powered off at
startup can be discovered without requiring a restart.

After enabling fields, restart `masterbus-signalk` so it reloads the TOML.

Then `nc localhost 3009` shows the live stream. Any language that can read a TCP
socket and parse JSON can consume this — Python, Node, shell, anything. Despite
the name it works fine without a Signal K server: it's just JSON lines.

## Writing values

For one-off changes (renaming switches, toggling an inverter, setting
a charge profile), `masterbus-tui` is the tool — navigate to the field,
press Enter, edit, Enter again to commit. Text fields, dropdowns,
booleans and numerics all round-trip.

For programmatic / scripted writes (e.g. switching a charger on from a
shell script or cron), the API is exposed through the
[`masterbus`](crates/masterbus) Rust crate. A few lines of Rust:

```rust
use masterbus::{Config, MasterBus, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Picks the transport from the per-host config file (auto-created on
    // first run; see `masterbus::FileConfig`).
    let bus = MasterBus::auto(Config::default())?;
    let device = bus.device(0x188EA2);          // your device id
    let field  = device.field(0x0013);          // your field id (three-digit hex from the TUI)
    field.set(Value::Boolean(true))?;            // turn it on
    Ok(())
}
```

For shell-script / one-off writes there's also a dedicated CLI:

    masterbus-set-field <device_id> <field_id> <value>

- `<device_id>` is the hex address from the TUI's title bar, e.g.
  `188EA2`.
- `<field_id>` is the three-digit hex shown next to each editable row in
  the TUI, e.g. `0x013` for a Btm1 field, `0x10E` for a Btm3 one (bit 8
  selects the channel).
- `<value>` is parsed against the field's type:
    - **boolean**: `true` / `false` / `on` / `off` / `1` / `0`
    - **number**: any decimal number
    - **list**: either the option index (`2`) or the exact label
      (`"Stabilized"`)
    - **text**: free string, capped at 16 printable-ASCII chars (the
      device-side limit; the TUI enforces the same cap on its editor)

Examples:

    masterbus-set-field 188EA2 0x013 on             # toggle a CombiMaster bool
    masterbus-set-field 3A3B4B 0x104 "Nav Chg"      # rename device name
    masterbus-set-field 53A493 0x160 "New Name"     # rename EasyView Switch 1

The TUI shows the *device id* (title bar, e.g. `[188EA2]`) and the
*field id* on every editable row, so picking the right ids is a
copy-paste away.
