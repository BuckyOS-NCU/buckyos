ARG NODE_IMAGE=node:24-bookworm-slim
FROM ${NODE_IMAGE} AS toolchain

ENV DEBIAN_FRONTEND=noninteractive \
    PATH=/root/.cargo/bin:/root/.local/bin:/root/.deno/bin:$PATH \
    UV_PYTHON=3.12 \
    UV_LINK_MODE=copy \
    CARGO_BUILD_JOBS=8 \
    CARGO_NET_GIT_FETCH_WITH_CLI=true

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates clang cmake curl git libclang-dev \
    libssl-dev pkg-config python3 python3-venv unzip xz-utils && \
    rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init -o /tmp/rustup-init && \
    chmod +x /tmp/rustup-init && \
    /tmp/rustup-init -y --profile minimal --default-toolchain stable && \
    rm /tmp/rustup-init
RUN curl -LsSf https://astral.sh/uv/install.sh -o /tmp/uv-install.sh && \
    sh /tmp/uv-install.sh && rm /tmp/uv-install.sh
RUN curl -fsSL https://deno.land/install.sh -o /tmp/deno-install.sh && \
    sh /tmp/deno-install.sh v2.9.2 && rm /tmp/deno-install.sh
ARG BUCKYOS_DEPENDENCY_REFRESH=main
RUN uv python install 3.12 && \
    uv tool install "buckyos-devkit @ git+https://github.com/buckyos/buckyos-devkit.git@main" && \
    npm install --global pnpm@10.13.1

FROM toolchain AS gateway
WORKDIR /workspace
RUN git clone --depth 1 --branch main https://github.com/buckyos/cyfs-gateway.git
WORKDIR /workspace/cyfs-gateway/src
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/workspace/cyfs-gateway/src/target \
    buckyos-build --app=cyfs-gateway --target=x86_64-unknown-linux-gnu && \
    buckyos-install --app=cyfs-gateway --all --target-rootfs=/opt/buckyos

FROM gateway AS source
WORKDIR /workspace/buckyos
COPY pyproject.toml ./
COPY .git ./.git
COPY src ./src
COPY publish ./publish
WORKDIR /workspace/buckyos/src
ARG NPM_REGISTRY=https://registry.npmjs.org
ENV npm_config_registry=$NPM_REGISTRY

FROM source AS build
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/tmp/rust_build/buckyos \
    --mount=type=cache,target=/root/.cache/uv \
    uv run buckyos-build.py --app=buckyos --target=x86_64-unknown-linux-gnu && \
    uv run buckyos-install.py --all --target-rootfs=/opt/buckyos && \
    deno run -A make_config.ts release --rootfs /opt/buckyos

ARG BUCKYOS_CONFIG_GROUP=release
RUN deno run -A make_config.ts "$BUCKYOS_CONFIG_GROUP" --rootfs /opt/buckyos

FROM source AS verify-auth
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/tmp/rust_build/buckyos \
    OPENSSL_STATIC=1 cargo test --release --target-dir /tmp/rust_build/buckyos \
    -p control_panel sys_auth_backend::tests -- --test-threads=1

FROM source AS verify-scripts
COPY build.py devenv.py build_aios build_exttool make_local_pkg.py /workspace/buckyos/
RUN install -m 755 /root/.local/bin/uv /usr/local/bin/uv && \
    cp -a /root/.local/share/uv/python /opt/python && \
    chown -R node:node /workspace/buckyos
USER node
ENV UV_PYTHON_INSTALL_DIR=/opt/python
RUN --mount=type=cache,target=/home/node/.cache/uv,uid=1000,gid=1000 \
    uv run python -m unittest discover -s tools/tests -p 'test_*.py'

FROM source AS verify-scheduler
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/tmp/rust_build/buckyos \
    OPENSSL_STATIC=1 cargo test --release --target-dir /tmp/rust_build/buckyos \
    -p scheduler -- --test-threads=1

FROM ${NODE_IMAGE} AS runtime
ENV DEBIAN_FRONTEND=noninteractive \
    BUCKYOS_ROOT=/opt/buckyos
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl docker.io iproute2 libssl3 procps python3 tini && \
    rm -rf /var/lib/apt/lists/*
COPY --from=build /opt/buckyos /usr/local/share/buckyos/rootfs
COPY docker/entrypoint.sh /usr/local/bin/buckyos-entrypoint
RUN chmod +x /usr/local/bin/buckyos-entrypoint && mkdir -p /opt/buckyos
WORKDIR /opt/buckyos
EXPOSE 80 443 2980 3180 3182
ENTRYPOINT ["/usr/bin/tini", "-g", "--", "/usr/local/bin/buckyos-entrypoint"]
