# syntax=docker/dockerfile:1

# 1. 编译阶段
FROM rust:1.85-alpine AS builder

WORKDIR /usr/src/app

# 安装 musl/gcc 编译 C 依赖 (如 bundled sqlite)，以及证书和时区数据。
# busybox-static 提供完全静态链接的 busybox (/usr/bin/busybox.static)，
# 供 scratch 运行阶段做 HEALTHCHECK；alpine 默认 busybox 是动态链接，
# 依赖 /lib/ld-musl-*.so.1，而 scratch 里没有该动态加载器，无法执行。
RUN apk add --no-cache musl-dev gcc pkgconfig ca-certificates tzdata busybox-static

# 复制源码并编译
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# 依赖说明：rust:1.85-alpine 的默认 host triple 即 x86_64/aarch64-unknown-linux-musl，
# musl 目标默认就是完全静态链接 (crt-static 默认开启)，无需也不应再设
# RUSTFLAGS="-C target-feature=+crt-static"：那会把 serde_derive 等 proc-macro
# (必须以宿主 .so 形式构建) 也强制静态，导致 Cargo 报
#   "cannot produce proc-macro ... does not support these crate types"
# 在 amd64 与 arm64 上均会构建失败。
#
# --locked：确保只用 Cargo.lock 锁定的版本（注意不要用 --frozen，它等价于
# --offline，首次构建 registry 缓存为空会导致拉取依赖失败）。
# cache mount：registry 与 target 目录跨构建持久化，改源码后只需增量重编。
# 注意：cache mount 的内容不会写入镜像层，因此必须把产物复制到普通路径 /out。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/app/target \
    cargo build --release --locked -p mdict-rs \
    && mkdir -p /out \
    && cp target/release/mdict-rs /out/mdict-rs

# 剥离符号表以进一步缩小镜像体积 (scratch 里无法执行 strip，必须在 builder 完成)
RUN strip /out/mdict-rs

# 创建非 root 运行时 (nobody, uid/gid 65534) 所需的 passwd/group 与目录。
# 词典目录必须可写：索引 .db 与生词本 favorites.db 都在该目录生成。
# 注意：COPY --from 不保留源阶段的属主/权限，必须用 --chown/--chmod 重新指定。
RUN echo 'nobody:x:65534:65534:nobody:/:/sbin/nologin' > /etc/passwd \
    && echo 'nobody:x:65534:' > /etc/group \
    && mkdir -p /app/mdict /tmp

# 2. 运行阶段 (极致精简 Scratch 镜像)
FROM scratch

WORKDIR /app

# 从 builder 复制证书和时区数据
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /usr/share/zoneinfo /usr/share/zoneinfo

# 非 root 运行所需的用户信息
COPY --from=builder /etc/passwd /etc/passwd
COPY --from=builder /etc/group /etc/group

# 从编译阶段复制完全静态链接的二进制和静态资源
COPY --from=builder /out/mdict-rs /app/mdict-rs
COPY --from=builder /usr/src/app/crates/mdict-server/resources/static /app/static
COPY --from=builder --chown=65534:65534 /app/mdict /app/mdict
# 注意：当前 Docker (29.x/BuildKit) 的 --chmod 对目录不生效 (实测 0777 仍得 755)，
# 所以 /tmp 直接 chown 给 nobody，保证应用可写。
COPY --from=builder --chown=65534:65534 /tmp /tmp

# 静态 busybox 提供 wget，供 scratch 中的 HEALTHCHECK 使用 (约 1MB)
COPY --from=builder /bin/busybox.static /bin/busybox

# 以非 root 用户 (nobody, 65534) 运行
USER nobody

EXPOSE 8181

ENV MDX_DICT_DIR=/app/mdict
ENV BIND_ADDR=0.0.0.0
ENV PORT=8181

# 注意：若挂载宿主机词典目录，需保证 nobody (uid 65534) 对该目录可写，
# 例如 chown -R 65534:65534 /path/to/dicts，否则索引 .db 与生词本会写入失败。
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/bin/busybox", "wget", "-q", "-O", "/dev/null", "http://127.0.0.1:8181/health"]

CMD ["/app/mdict-rs"]
