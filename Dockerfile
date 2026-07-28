# 1. 编译阶段
FROM rust:1.85-alpine AS builder

WORKDIR /usr/src/app

# 安装 musl/gcc 编译 C 依赖 (如 bundled sqlite)
RUN apk add --no-cache musl-dev gcc pkgconfig

# 复制源码并编译
COPY Cargo.toml Cargo.loc[k] ./
COPY crates ./crates

RUN cargo build --release -p mdict-rs

# 2. 运行阶段
FROM alpine:latest

WORKDIR /app

# 安装基础运行时依赖
RUN apk add --no-cache ca-certificates tzdata

# 从编译阶段复制二进制和静态资源
COPY --from=builder /usr/src/app/target/release/mdict-rs /app/mdict-rs
COPY --from=builder /usr/src/app/crates/mdict-server/resources/static /app/static

# 创建默认词典存放目录
RUN mkdir -p /app/mdict

EXPOSE 8181

ENV MDX_DICT_DIR=/app/mdict

CMD ["/app/mdict-rs"]
