# masterbus-tools

Three command-line tools for Mastervolt **MasterBus**, built on the
[`masterbus`](https://crates.io/crates/masterbus) library. Install all
of them in one go:

    cargo install masterbus-tools

Each binary works over **Linux SocketCAN** or — on Linux, macOS, and
Windows — over the **Mastervolt USB link** (a class-compliant HID
device, no vendor driver needed).

## Configuration

All three tools share a small INI file describing the transport and the
optional "act as bus master" role:

| OS | Config path | Default cache dir |
|----|------|------|
| Linux (system) | `/etc/default/masterbus/config.ini` (if writable) | `/var/lib/masterbus` |
| Linux (user) | `$XDG_CONFIG_HOME/masterbus/config.ini` (default `~/.config/...`) | `$XDG_CACHE_HOME/masterbus` (default `~/.cache/...`) |
| macOS | `~/Library/Application Support/masterbus/config.ini` | `~/Library/Caches/masterbus` |
| Windows | `%APPDATA%\masterbus\config.ini` | `%LOCALAPPDATA%\masterbus\cache` |

The file is **read on every start** and **auto-created on first run**
with sensible defaults — a Mastervolt USB link if plugged in, otherwise
the lone CAN interface. The chosen path and detected values are logged
to stderr on creation.

```ini
heartbeat_master = 000001               # 24-bit hex; comment out to stay passive
device_type      = can                  # "usb" or "can"
device_name      = can0                 # CAN iface, or USB-link serial (blank = first)
cache_dir        = /var/lib/masterbus   # schema cache; comment out to disable
```

Multiple CAN interfaces with no USB link is an error — edit the file
and pick one. To switch transports, change the master role, or relocate
the cache, edit the file (or delete it to re-auto-detect). If the
configured `cache_dir` isn't writable by the running user (e.g.
`/var/lib/masterbus` for an unprivileged shell), the engine silently
falls back to the OS-native per-user cache directory.

## `masterbus-tui`

Terminal UI for browsing devices, viewing live values, and editing
writable fields.

    masterbus-tui

Devices are listed on the left with liveness; the selected device's
groups and fields are on the right. `Tab` / `Shift-Tab` switch between
the Summary / Monitoring / Configuration / Service / Settings tabs (each
discovered on demand). `Enter` edits a writable field — booleans toggle,
numbers / lists / text open a centred edit modal. `l` opens the
access-level (login) modal — higher levels unlock more fields. `q`
quits.

## `masterbus-signalk`

[Signal K](https://signalk.org) sidecar: discovers MasterBus devices and their
monitoring fields, converts supported values to SI units, and serves Signal K
deltas as newline-delimited JSON over TCP (default `0.0.0.0:3009`).

    masterbus-signalk [listen-addr]
    # e.g.: masterbus-signalk                # default port (0.0.0.0:3009)
    #       masterbus-signalk 0.0.0.0:4000   # bind elsewhere

The built-in mapper supplies sensible Signal K paths for supported MasterBus
device classes and fields. Discovery is intentionally separate from
publication: **nothing is published merely because it was discovered**.
Publication is controlled per field by `masterbus-signalk-fields.toml`.

Sample delta:

```json
{"updates":[{"$source":"masterbus","timestamp":"2026-05-25T18:00:00.000Z","values":[{"path":"electrical.batteries.main-batt-4.voltage","value":26.6}]}]}
```

### Field publication configuration

On discovery, `masterbus-signalk` creates or updates
`masterbus-signalk-fields.toml` in the current working directory. The file is
installation-specific and is intentionally excluded from Git.

Every newly discovered field defaults to:

```toml
enabled = false
```

so a user must explicitly choose what is sent to Signal K.

A field entry looks like:

```toml
[[fields]]
device = "0x123456"
class = "CHG"
instance = "fwd-charger"
group = "monitoring"
index = "2"
field = "Output 1"
unit = "V"
enabled = false
suggested_path = "electrical.chargers.fwd-charger.voltage"
path = "electrical.chargers.fwd-charger.voltage"
```

`device` is the stable MasterBus device address and `index` identifies the
field on that device. Those values are used to preserve the user's choices
across rediscovery even if a device name changes.

`suggested_path` is maintained by the built-in mapper. `path` is the effective
Signal K path and may be edited freely. To publish the field, set
`enabled = true`.

For example:

```toml
enabled = true
suggested_path = "electrical.chargers.fwd-charger.voltage"
path = "electrical.chargers.fwdAC.voltage"
```

publishes that value as `electrical.chargers.fwdAC.voltage`.

User path choices are not overwritten when discovery runs again. Newly
discovered fields are added disabled. Fields that the built-in mapper does not
yet recognize are also recorded in the configuration with an empty suggested
path; a user can assign a custom `path` and explicitly enable them without
changing the Rust mapper.

### Static device metadata

Static Signal K metadata is independently opt-in per device:

```toml
[[devices]]
device = "0x123456"
class = "CHG"
instance = "fwd-charger"
publish_name = true
publish_manufacturer_name = false
publish_model = true
```

`name`, `manufacturer.name`, and `manufacturer.model` can therefore be enabled
or disabled independently.

Metadata follows the effective user-configured Signal K base path derived from
enabled fields. For example, if a user changes a field from the suggested
`electrical.chargers.fwd-charger...` path to
`electrical.chargers.fwdAC...`, enabled static metadata for that device is
published under `electrical.chargers.fwdAC...` as well. Vessel- or
installation-specific names therefore do not need to be hard-coded into the
mapper.

### Discovery and rediscovery

At startup the sidecar performs an initial MasterBus discovery, builds the
field inventory, and updates the TOML configuration.

It then continues discovery while running so a device that was not powered or
present during startup can be added later. New devices and fields are added to
the configuration with publication disabled by default.

A previously discovered device does not need to be rediscovered simply because
it temporarily stops sending data. While it is absent there are no new values
to publish; when it resumes sending, its configured fields can resume normally.

### Run as a systemd service

A hardened unit is included at
[`etc/masterbus-signalk.service`](etc/masterbus-signalk.service):

```sh
sudo cp $(which masterbus-signalk) /usr/local/bin/             # already there if installed via cargo install
sudo cp etc/masterbus-signalk.service /etc/systemd/system/
# First run as root creates /etc/default/masterbus/config.ini with
# auto-detected transport + master settings; review/edit it as needed.
sudo systemctl enable --now masterbus-signalk
```

Transport, master role, and the schema-cache directory are configured in
`/etc/default/masterbus/config.ini` (auto-created on first run; see the
**Configuration** section above) and the systemd unit's `LISTEN` env var. The
service keeps a persistent schema cache in `/var/lib/masterbus` and restarts on
failure.

## `masterbus-set-field`

One-shot CLI to write a single field — handy from shell scripts and
cron:

    masterbus-set-field <device_id> <field_id> <value>

- `<device_id>`: hex 24-bit address from the TUI's title bar, e.g. `188EA2`.
- `<field_id>`: three-digit hex from the TUI field list, e.g. `0x013`
  (Btm1) or `0x10E` (Btm3 — bit 8 selects the channel).
- `<value>`: parsed per the field's type — boolean (`true`/`false`/`on`/
  `off`/`1`/`0`), number, list index *or* exact option label, or free
  text (max 16 printable-ASCII bytes for editable strings, the wire
  limit).

Examples:

    masterbus-set-field 188EA2 0x013 on               # CombiMaster bool
    masterbus-set-field 3A3B4B 0x104 "Nav Chg"        # Magic Nav Chg rename
    masterbus-set-field 53A493 0x160 "Schakelaar"     # EasyView Switch 1

## License

Apache-2.0.
