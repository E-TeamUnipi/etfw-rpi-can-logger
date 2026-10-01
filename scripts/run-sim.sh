#!/bin/sh
# Run the logger stack with simulated CAN traffic.
#   Linux: natively.  macOS (or --docker): in a Linux container.
# Status page and API on http://127.0.0.1:8080. For the analysis app, serve
# webapp/ locally (webapp/build.sh once, then e.g.
# `python3 -m http.server 8000 -d webapp`) and open http://localhost:8000.
# The simulated logger allows sending (PIN 1234).
set -e
cd "$(dirname "$0")/.."

if [ "$1" = --docker ] || [ "$(uname -s)" != Linux ]; then
	# copy the checkout in (no bind mount: Docker Desktop may not share its folder)
	docker volume create canlogger-sim-src >/dev/null
	COPYFILE_DISABLE=1 tar --exclude ./target --exclude './output-*' --exclude ./.git --exclude ./webapp/pkg -cf - . |
		docker run --rm -i -v canlogger-sim-src:/src rust:1-slim sh -c 'rm -rf /src/* && tar -C /src -xf - 2>/dev/null'
	TTY=
	[ -t 1 ] && TTY=-t
	exec docker run --rm -i $TTY -p 127.0.0.1:8080:8080 \
		-v canlogger-sim-src:/src -v canlogger-sim-target:/target -v canlogger-sim-cargo:/usr/local/cargo/registry \
		-e CARGO_TARGET_DIR=/target -w /src rust:1-slim sh -c '
			set -e
			cargo build --release -p canlogd -p canweb -p canlog-core
			W=/tmp/sim; mkdir -p $W
			/target/release/canring $W/ring.img create 256
			printf "device_name = CAN logger (simulated)\napp_origin = http://localhost:8000, https://e-teamunipi.github.io\ncontrol_pin = 1234\n" > $W/logger.conf
			/target/release/canlogd --sim -c $W/logger.conf -r $W/ring.img -s $W/ctl.sock &
			sleep 0.5
			echo "open http://127.0.0.1:8080  (Ctrl-C to stop)"
			exec /target/release/canweb -c $W/logger.conf -r $W/ring.img -s $W/ctl.sock -l 0.0.0.0:8080'
fi

cargo build --release -p canlogd -p canweb -p canlog-core
W=${TMPDIR:-/tmp}/canlogger-sim
mkdir -p "$W"
[ -f "$W/ring.img" ] || ./target/release/canring "$W/ring.img" create 256
[ -f "$W/logger.conf" ] || printf 'device_name = CAN logger (simulated)\napp_origin = http://localhost:8000, https://e-teamunipi.github.io\ncontrol_pin = 1234\n' > "$W/logger.conf"
./target/release/canlogd --sim -c "$W/logger.conf" -r "$W/ring.img" -s "$W/ctl.sock" &
LOGGER=$!
trap 'kill $LOGGER 2>/dev/null' EXIT INT TERM
sleep 0.5
echo "open http://127.0.0.1:8080  (Ctrl-C to stop)"
./target/release/canweb -c "$W/logger.conf" -r "$W/ring.img" -s "$W/ctl.sock" -l 127.0.0.1:8080
