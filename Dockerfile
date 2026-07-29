# 1. 编译阶段
FROM rust:1.85-alpine AS builder

WORKDIR /usr/src/app

# 安装 musl/gcc 编译 C 依赖 (如 bundled sqlite)，以及证书和时区数据
RUN apk add --no-cache musl-dev gcc pkgconfig ca-certificates tzdata

# 复制源码并编译
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# 依赖说明：rust:1.85-alpine 的默认 host triple 即 x86_64/aarch64-unknown-linux-musl，
# musl 目标默认就是完全静态链接 (crt-static 默认开启)，无需也不应再设
# RUSTFLAGS="-C target-feature=+crt-static"：那会把 serde_derive 等 proc-macro
# (必须以宿主 .so 形式构建) 也强制静态，导致 Cargo 报
#   "cannot produce proc-macro ... does not support these crate types"
# 在 amd64 与 arm64 上均会构建失败。
RUN cargo build --release -p mdict-rs
# 剥离符号表以进一步缩小镜像体积 (scratch 里无法执行 strip，必须在 builder 完成)
RUN strip target/release/mdict-rs

# 创建供运行时挂载词典的空目录
RUN mkdir -p /app/mdict

# 2. 运行阶段 (极致精简 Scratch 镜像)
FROM scratch

WORKDIR /app

# 从 builder 复制证书和时区数据
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /usr/share/zoneinfo /usr/share/zoneinfo

# 从编译阶段复制完全静态链接的二进制和静态资源
COPY --from=builder /usr/src/app/target/release/mdict-rs /app/mdict-rs
COPY --from=builder /usr/src/app/crates/mdict-server/resources/static /app/static
COPY --from=builder /app/mdict /app/mdict

EXPOSE 8181

ENV MDX_DICT_DIR=/app/mdict
ENV BIND_ADDR=0.0.0.0
ENV PORT=8181

# 注意：Scratch 镜像没有 shell 和 wget，因此无法使用依赖命令行的 HEALTHCHECK。
# 如果使用 Kubernetes 等编排工具，请配置 HTTP Probe 直接访问服务的 /health 接口。

CMD ["/app/mdict-rs"]
