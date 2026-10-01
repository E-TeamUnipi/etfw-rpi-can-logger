# Build environment for scripts/build-image.sh --docker.
# Built for the host's own architecture (amd64 or arm64): no emulation.
FROM debian:trixie

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
	bash bc binutils build-essential bzip2 ca-certificates cpio diffutils file \
	findutils gawk git gzip libncurses-dev make patch perl python3 rsync sed \
	tar unzip wget xz-utils \
	&& rm -rf /var/lib/apt/lists/*

# Buildroot does not need root; the cache volume is owned by this user.
RUN useradd -m -u 1000 builder
USER builder
WORKDIR /home/builder
