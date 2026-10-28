# Build image for the Linux release archives, used by cross for both targets.
#
# Ubuntu 20.04 sets the glibc floor at 2.31, so the binaries run on Ubuntu 20.04+,
# Debian 11+ and RHEL 9+. It is old enough for that and new enough to ship a
# libclang that bindgen accepts (ruby-prism-sys runs bindgen at build time),
# which cross's own 0.2.5 images, based on Ubuntu 16.04, do not.
FROM ubuntu:20.04@sha256:8feb4d8ca5354def3d8fce243717141ce31e2c428701f6682bd2fafe15388214

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install --assume-yes --no-install-recommends \
      ca-certificates curl make cmake pkg-config \
      gcc g++ libc6-dev \
      clang libclang-dev \
      gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross \
    && apt-get clean \
    && find /var/lib/apt/lists -type f -delete

# bindgen (ruby-prism-sys) parses C headers with libclang. For an aarch64 target
# clang searches /usr/include/aarch64-linux-gnu, then /usr/include, which holds the
# x86_64 libc headers and cannot find sys/cdefs.h. Debian's cross libc keeps the
# aarch64 headers in /usr/aarch64-linux-gnu/include, so expose them at the
# multiarch path clang looks in first. Setting BINDGEN_EXTRA_CLANG_ARGS instead
# does not work: ruby-prism-sys writes its own value for the dashed target name,
# which bindgen reads before any other.
RUN ln -s /usr/aarch64-linux-gnu/include /usr/include/aarch64-linux-gnu \
    && test -f /usr/include/aarch64-linux-gnu/sys/cdefs.h

# Only the aarch64 target uses these; the x86_64 build uses the native gcc.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
