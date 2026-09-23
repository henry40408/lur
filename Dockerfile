# syntax=docker/dockerfile:1

# ---- build: cross-compile a static musl binary with cargo-zigbuild ----------
# Native build platform + zig cross-compile: no qemu. The Rust version comes from
# rust-toolchain.toml; don't pin e.g. `rust:1.97` — un-suffixed tags resolve to
# trixie, a silent Debian major bump.
FROM --platform=$BUILDPLATFORM rust:bookworm AS build

# CMake for aws-lc-sys (rustls backend); curl + xz fetch zig.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake curl xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Zig 0.14.1 avoids the libc++-19 bindgen requirement that 0.15+ introduces.
ARG ZIG_VERSION=0.14.1
# >= 0.23.0 drops `-Wl,--fix-cortex-a53-843419`, which rustc emits for aarch64
# since 1.98 (rust-lang/rust#155453) and zig's linker rejects.
ARG ZIGBUILD_VERSION=0.23.0
RUN cargo install cargo-zigbuild --version "${ZIGBUILD_VERSION}" --locked
RUN set -eux; \
    case "$(uname -m)" in \
      x86_64) zarch=x86_64 ;; \
      aarch64) zarch=aarch64 ;; \
      *) echo "unsupported build arch $(uname -m)" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-${zarch}-linux-${ZIG_VERSION}.tar.xz" \
      | tar -xJ -C /opt; \
    ln -s "/opt/zig-${zarch}-linux-${ZIG_VERSION}/zig" /usr/local/bin/zig

WORKDIR /app

# Toolchain layer keyed on rust-toolchain.toml alone; `cargo --version` triggers
# the rustup install.
COPY rust-toolchain.toml .
RUN cargo --version

COPY . .

# GIT_VERSION stamps `lur --version` via build.rs; with no .git in the context,
# "dev" ends up as "dev".
ARG TARGETARCH
ARG GIT_VERSION=dev
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target,sharing=locked \
    set -eux; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported target arch $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    export GIT_VERSION="${GIT_VERSION}"; \
    rustup target add "$target"; \
    cargo zigbuild --release --target "$target"; \
    install -Dm755 "target/${target}/release/lur" /out/lur

# ---- runtime: minimal static image (CA certs + nonroot, no shell) -----------
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/lur /usr/local/bin/lur

# The default bind is loopback; containers need all interfaces. ENV keeps it
# overridable (`-e BIND=...`).
ENV BIND=0.0.0.0:8080
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/lur"]
