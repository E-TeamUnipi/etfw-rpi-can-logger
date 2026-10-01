#!/bin/sh
# Run the logger stack on a Linux laptop with simulated CAN traffic.
# Opens the web page on http://127.0.0.1:8080
set -e
cd "$(dirname "$0")/.."
cargo build --release -p canlogd -p canweb -p canlog-core
W=${TMPDIR:-/tmp}/canlogger-sim
mkdir -p "$W"
[ -f "$W/ring.img" ] || ./target/release/canring "$W/ring.img" create 256
[ -f "$W/logger.conf" ] || printf 'device_name = CAN logger (simulated)\n' > "$W/logger.conf"
./target/release/canlogd --sim -c "$W/logger.conf" -r "$W/ring.img" -s "$W/ctl.sock" &
LOGGER=$!
trap 'kill $LOGGER 2>/dev/null' EXIT INT TERM
sleep 0.5
echo "open http://127.0.0.1:8080  (Ctrl-C to stop)"
./target/release/canweb -c "$W/logger.conf" -r "$W/ring.img" -s "$W/ctl.sock" -l 127.0.0.1:8080
