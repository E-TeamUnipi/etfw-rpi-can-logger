# CAN logger for Raspberry Pi 3 B / 4 / 5

A Raspberry Pi that boots in a few seconds and logs every frame from every
USB-CAN adapter plugged into it. It survives power cuts because it has no
filesystem to corrupt. An app on your phone or laptop decodes the traffic
with your DBC files, plots signals, analyses bus load and response times, and
can send frames.

```
 USB-CAN adapters ◄─► canlogd ──► raw ring partition on the SD card (p3)
                        │  ▲
          control socket│  │power-fail GPIO (12 V comparator)
                        ▼
       canweb (Wi-Fi ET-18, http://192.168.4.1)     canble (Bluetooth LE)
       status page + API: status, frame stream,     status · ~1 Hz live data ·
       downloads, commands, sending                 commands, sending
                        ▲                                ▲
                        └──────── webapp/ ───────────────┘
              analysis app (PWA on GitHub Pages, works offline):
              DBC decoding · plots · bus load · response time · send
```

| Part | What it does |
|---|---|
| `crates/canlog-core` | On-disk ring format, head search, session list, candump/ASC export, `canring` CLI |
| `crates/canlogd` | The logger. The only process that writes to the ring. |
| `crates/canweb` | Status page on the hotspot, and the HTTP API the app uses (CORS for the app's origin) |
| `crates/canble` | BLE GATT service: status and live data at 1 Hz, time sync, commands, Wi-Fi on/off |
| `crates/canlog-dbc` | DBC parser, signal decoder/encoder (multiplexing, CAN FD, float signals) |
| `crates/canlog-analysis` | Frame timing on the wire, log import, bus load, response-time analysis, plot series |
| `crates/canlog-wasm` | The two crates above compiled to WebAssembly for the app |
| `webapp/` | The app: vanilla JS + the wasm engine, installable, works offline |
| `buildroot/` | BR2_EXTERNAL tree: defconfigs for Pi 3 B, Pi 4 and Pi 5, kernel fragment, init, services |

If `canweb` or `canble` crashes, recording continues. `canlogd` is restarted by
init if it ever exits, and a hardware watchdog reboots the Pi if it hangs.

---

## 1. Build the SD card image

```sh
scripts/build-image.sh rpi4     # or rpi3, rpi5
```

The result is `output-rpi4/images/sdcard.img`. The build runs natively on
x86_64 and arm64 hosts: the toolchain is the Arm GNU toolchain, which is
prebuilt for both, so the image is the same whichever machine builds it.

- **Linux** (x86_64 or arm64): builds directly. You need Buildroot's usual
  host packages (`build-essential`, `rsync`, `bc`, `cpio`, `unzip`, `file`,
  `wget`, `git`, `libncurses-dev`) and internet access.
- **macOS** (or `--docker` anywhere): builds in a Docker container for the
  host's own architecture, arm64 on Apple Silicon, with no emulation. You
  need Docker Desktop. The checkout is copied into the container, so the
  repo's folder does not need to be shared with Docker.

Caches are kept between builds: downloads, the compiler cache (ccache) and
Rust crates, in `~/.cache/canlogger` on Linux and in the Docker volume
`canlogger-br` on macOS. Packages build in parallel, one per CPU. A second
board, or a rebuild after an edit, only compiles what changed. The Rust
workspace is re-synced from the checkout on every run, so after changing
Rust code you just run the script again.

Options: `--clean` starts a board's output from scratch (needed after
changing the toolchain or CPU in a defconfig), `-j N` sets the number of
jobs, and extra arguments are passed to Buildroot's `make`, for example
`scripts/build-image.sh rpi4 linux-menuconfig`.

A failed download is retried twice before the build gives up. Buildroot
resumes where it stopped, so a retry costs nothing.

GitHub Actions builds all three images on a manual run or on a `v*` tag
(`.github/workflows/image.yml`). The compressed images are attached to the
run as artifacts.

**Flash** with Raspberry Pi Imager ("Use custom") or
`sudo dd if=sdcard.img of=/dev/sdX bs=4M conv=fsync`
(macOS: `diskutil unmountDisk /dev/diskN`, then `of=/dev/rdiskN bs=4m`).

The Pi 3, Pi 4 and Pi 5 images are separate. Each uses the official
kernel configuration for its board. The Pi 3 image runs a 64-bit kernel
and also boots the Pi 3 B+.

### One-time EEPROM settings (fastest boot)

The Pi 3 has no EEPROM and already boots from the SD card: skip this.

On a Pi 4 / Pi 5, do this once per Pi, from Raspberry Pi OS: run `sudo rpi-eeprom-config --edit`
and set:

```
BOOT_ORDER=0xf1       # SD card only: no USB / network probing
DISABLE_HDMI=1        # skip the HDMI diagnostics screen
BOOT_UART=0
# Pi 5 on a non-official supply, if you use several USB devices:
#PSU_MAX_CURRENT=5000
```

## 2. First boot

1. On first boot, the logger creates partition 3 (the raw ring) in the free
   space of the card and formats it. Every later boot finds the newest
   block in a few milliseconds and continues after it.
2. The Wi-Fi hotspot `ET-18` comes up (password from `local.conf`, below),
   and the BLE service advertises as `CANLog-XXXX`.
3. Join the hotspot and open **http://192.168.4.1** (any `http://` address
   works) for the status page, or use the app (section 5). Both sync the
   logger's clock from your phone automatically.

### Passwords and PINs stay out of git

Secrets go in `buildroot/board/canlogger/local.conf`, which git ignores.
Copy `local.conf.example` and fill it in before building:

```
wifi_password = ...          # or wifi_psk = <64 hex digits>
control_pin = ...            # needed for sending frames and BLE commands
root_password = ...          # SSH; hashed into /etc/shadow at build time
```

The `logger.conf` keys in it are appended to the `logger.conf` on the boot
partition. Without `root_password`, root has no password and SSH works only
with a key in `buildroot/board/canlogger/authorized_keys` (also ignored by
git). Without `wifi_password` the Wi-Fi password is `canlogger`.

`wifi_psk` is the hashed form of the Wi-Fi password (`wpa_passphrase ET-18
'password'` prints it). It hides the readable password, but it is still the
network key: anyone who has it can join.

For images built by GitHub Actions, store the content of your `local.conf`
as the repository secret `CANLOGGER_LOCAL_CONF`.

You can also edit `logger.conf` on the card itself. For SSH, run
`ssh root@192.168.4.1`.

## 3. Configuration: `logger.conf`

`logger.conf` lives on the **FAT boot partition**, next to `config.txt`. Put
the card in any computer, edit the file, and power-cycle the Pi.

The main settings are:

- `can.bitrate`, `can.dbitrate`: defaults for every adapter.
- `can.<serial|usb-port|ifname>.bitrate / .label`: per-adapter settings. The
  web page shows each adapter's serial number and USB port, so you can pin a
  physical port or a specific adapter to a bus.
- `can.listen_only = 1` (the default): the logger never transmits or ACKs.
  Leave this on in a car.
- `powerfail_gpio`: the GPIO wired to your 12 V comparator (see below).
- `wifi = on|off`: with `off`, the hotspot only starts when you switch it on
  over Bluetooth.
- `control_pin`: Bluetooth commands and sending frames (Wi-Fi or Bluetooth)
  must carry this PIN. Sending is refused while no PIN is set.
- `can.<serial|usb-port|ifname>.tx = 1`: allow sending frames on that
  adapter (see "Sending frames").
- `app_origin`: web pages allowed to call the HTTP API from a browser (the
  app's GitHub Pages address).

The file itself documents every key.

## 4. Getting data out

- **The app** (section 5): "Open in app" on a recorded session, or download
  it as a file.
- **Hotspot web page.** Each session has **candump .log** and **Vector .asc**
  downloads, gzip-compressed by default, with an optional "last N minutes"
  range. candump logs open in can-utils, python-can, SavvyCAN and cantools.
  ASC opens in CANalyzer/CANoe, python-can and asammdf.
- **Card in a laptop (Linux).** Build `canring` with
  `cargo build --release -p canlog-core`, then:
  ```sh
  sudo ./target/release/canring /dev/sdX3 list
  sudo ./target/release/canring /dev/sdX3 export last -f asc -o auto
  ```
  On macOS, use `/dev/rdiskNs3`.
- **On the Pi.** Run `canring /dev/mmcblk0p3 list`.

## 5. The app

The app is a static web page published to GitHub Pages
(`.github/workflows/pages.yml`, https://e-teamunipi.github.io/etfw-rpi-can-logger/).
Open it once while online and install it ("Install" / "Add to Home screen").
It then works offline.

**Getting data in:**

- **Wi-Fi:** join `ET-18` and press "Connect over Wi-Fi". Chrome asks once to
  allow access to devices on your local network: allow it. This uses Chrome's
  Local Network Access, which lets the HTTPS app call `http://192.168.4.1`.
  You get every frame live, session downloads, "Open in app" and sending.
  On macOS also allow Chrome under System Settings → Privacy & Security →
  Local Network (and Bluetooth).
- **Bluetooth:** status, ~1 Hz snapshots of the last frame per id, commands,
  sending.
- **Log files:** candump `.log` or Vector `.asc`, optionally `.gz`. They are
  read in the browser and never uploaded.

**Tabs:**

- **Buses:** bus profiles (name, bitrate, one or more DBC files; a CAN FD data bitrate is read from the DBC). Name a
  profile like the adapter label (`can.<serial>.label`) and live data and
  sessions from the logger map to it automatically. Other log buses are
  mapped once in the Data tab. DBCs, profiles and workbooks are stored only in
  this browser. "Export workspace" moves them to another device.
- **Data:** what is loaded, bus to profile mapping, and the latest frame of
  every message, decoded.
- **CAN analysis:**
  - measured bus load: average, and peaks over 10 ms, 100 ms and 1 s
    windows. Frame lengths count the real stuff bits.
  - per-message timing: period, jitter, largest gap compared with the DBC
    cycle time.
  - **response time from the DBC**, per message, from release to end of
    transmission:
    - worst case: the CAN schedulability analysis of Davis, Burns, Bril and
      Lukkien (2007), with worst-case stuffing, blocking and interference;
    - average: a bus simulation with random phases.

    Event messages get a minimum gap typed into the table.
- **Plots:** workbooks of plot panels, saved locally. Live, they scroll; on a
  log, drag to zoom and double-click to zoom out.
- **Send:** frames built from the DBC (signal values, multiplexing) or raw.

| Browser | Log files, analysis, plots | Bluetooth | Wi-Fi to the logger |
|---|---|---|---|
| Chrome / Edge (Android, desktop) | yes | yes | yes |
| Safari (Mac), iPhone | yes (install to Home Screen) | no | no: use http://192.168.4.1, download, then open the file |
| Bluefy (iPhone) | yes | yes | no |

### Sending frames

Sending is allowed only on adapters with `can.<id>.tx = 1`, and only with
the `control_pin`. The logger is listen-only by default: it never transmits
or ACKs.

- The first frame sent on an adapter switches it out of listen-only, in
  one-shot mode (no automatic retransmission). From then on it **ACKs every
  frame on that bus**.
- It stays that way until you press "Back to listen-only" in the app, or the
  logger restarts.
- The switch is written to the log as a marker. The interface is down for a
  moment while it switches.
- Frames you send are recorded like any other frame and marked `Tx` in ASC
  exports.

### Building and testing the app locally

```sh
webapp/build.sh                          # wasm engine -> webapp/pkg (needs wasm-pack)
python3 -m http.server 8000 -d webapp    # http://localhost:8000
scripts/run-sim.sh                       # simulated logger on :8080 (Docker on macOS)
```

In the app, connect over Wi-Fi to `127.0.0.1:8080` (PIN `1234`). DBCs
matching the simulated traffic are in `crates/canlogd/testdata/`
(profiles "Powertrain (simulated)" and "Body (simulated)").

## 6. Power-fail input and hold-up

Put the hold-up on the **12 V side**, before the 5 V converter:

```
12 V ──►|── diode ──┬── 5 V buck ── Pi
                    │
                  ~1 F / 16 V supercap (or bank), with inrush limiting
12 V (before diode) ── divider ── comparator with hysteresis / optocoupler ── GPIO17 (pin 11)
```

- The Pi 4 draws about 5 W. 2 s of hold-up from 12 V down to 7 V is roughly
  0.25 F, so 1 F leaves margin for the Pi 5 and the USB adapters.
- The comparator output is **low when the supply is lost** (the default,
  `powerfail_active_low = 1`).
- **When the supply drops**, canlogd writes its current block (flagged
  "power fail") and stops writing, then keeps buffering frames in RAM.
- **When the supply returns** and stays up for `powerfail_restore_ms`, which
  covers engine cranking, the logger writes the buffer out and carries on.
  Nothing is lost.

## 7. Editing the system

- **Settings:** edit `logger.conf` or `config.txt` on the boot partition.
- **Quick experiments on the Pi:** run `rw`, edit, then `ro`. These are shell
  aliases for remounting the rootfs. Copy the change into
  `buildroot/board/canlogger/rootfs_overlay` afterwards, or the next flash
  undoes it.
- **Logs:** run `logread -f`. All services log to an in-RAM syslog.
- **Serial console:** see the comments at the top of `config.txt`.
- **Proper changes:** edit this repo and rebuild the image.

## 8. How the ring works

- Partition 3 is split into 4 KiB blocks. Block 0 is a superblock with a
  random ring id.
- The block with sequence number `s` always lives at position `s mod N`.
  Each block carries a CRC, the ring id, the session id, the wall-clock
  offset and the session name.
- **On boot**, a binary search finds the newest valid block in about 25
  reads. Nothing is mounted and nothing is repaired.
- **A block torn by a power cut** fails its CRC and is skipped. It can never
  affect other blocks.
- **Writes** are sequential, O_DIRECT and O_DSYNC, batched up to 64 blocks
  per system call, and happen at least every `flush_ms`. At most about 1 s
  of data sits in RAM.
- **Every 30 s** the logger writes a checkpoint (interface list, time sync,
  name). A session whose start was overwritten after the ring wrapped can
  still be exported with interface names and real timestamps.
- **Readers** (web, CLI) open the partition read-only and enumerate sessions
  by binary search on the session id. A 30 GB ring lists instantly.
- **When the ring is full**, the oldest data is overwritten.

## 9. Trying it without hardware

```sh
scripts/run-sim.sh      # simulated CAN traffic, web page/API on :8080 (Docker on macOS)
cargo test              # ring format, exports, DBC, analysis (canlog-core/canlogd need Linux)
```

## Status and known gaps

The following has been **tested on a PC**:

- ring format and recovery (unit tests, plus repeated hard kills during
  recording)
- ring wrap with concurrent readers
- power-fail hold (simulated)
- the web UI
- the app over Wi-Fi against the simulator (headless Chrome): live stream,
  DBC decoding, plots, bus load, response time, sending, session import
- the BLE payloads
- candump/ASC output, verified by parsing with python-can
- the Buildroot defconfigs: every symbol resolves in Buildroot 2026.08
- the kernel config fragment: every symbol exists in the pinned Raspberry Pi
  kernel (6.12.61), `krnbt` is valid on both boards, and all listed device
  trees exist
- the package's vendor-then-offline cargo build

The following has **not been run on a Pi yet**:

- the full Buildroot image build
- real USB-CAN adapters
- the GPIO power-fail input
- BlueZ advertising, and the app over Bluetooth
- hostapd/dnsmasq
- sending on a real adapter (listen-only/one-shot switching, `MSG_DONTROUTE`
  marking of our own frames)
- actual boot time

Expect to adjust small things on first bring-up. The likeliest are Wi-Fi and
Bluetooth firmware details and the exact `config.txt` flags.
