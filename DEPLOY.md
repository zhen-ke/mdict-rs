# MDict 词典 - Armbian 部署指南

## 📦 环境准备 (macOS)

### 1. 安装交叉编译工具链

```bash
# 安装 musl-cross (ARM64 交叉编译器)
brew install FiloSottile/musl-cross/musl-cross

# 添加 Rust 目标平台
rustup target add aarch64-unknown-linux-musl
```

### 2. 配置 Cargo

创建 `.cargo/config.toml`:

```toml
[target.aarch64-unknown-linux-musl]
linker = "aarch64-linux-musl-gcc"
```

### 3. 编译

```bash
cargo build --release --target aarch64-unknown-linux-musl
```

## 🚀 快速部署

```bash
# 一键部署 (部署到 /DATA/Documents/mdict-server)
./deploy.sh root@armbian-ip

# 上传词典文件
scp 词典.mdx 词典.mdd root@armbian:/DATA/Documents/mdict-server/mdict/

# 重启服务
ssh root@armbian "sudo systemctl restart mdict"
```

## 📁 目录结构

```
/DATA/Documents/mdict-server/
├── mdict-rs              # 二进制
├── static/               # 静态资源
└── mdict/                # 词典 ← 放这里
    ├── xxx.mdx
    └── xxx.mdd
```

## 📋 常用命令

```bash
# 查看状态
sudo systemctl status mdict

# 重启服务
sudo systemctl restart mdict

# 查看日志
journalctl -u mdict -f

# 查看内存占用
ps aux | grep mdict-rs
```

## 🌐 访问

浏览器打开: `http://armbian-ip:8181`

## 🔧 更换词典

```bash
# 上传新词典
scp 新词典.mdx root@armbian:/DATA/Documents/mdict-server/mdict/

# 上传静态资源
scp resources/static/* root@armbian:/DATA/Documents/mdict-server/static/

# 上传二进制
scp target/aarch64-unknown-linux-musl/release/mdict-rs root@armbian:/DATA/Documents/mdict-server/

# 重启服务 (自动建索引)
sudo systemctl restart mdict

# 停止服务
systemctl stop mdict

# 查看日志
journalctl -u mdict -fdict
```

## 🔍 故障排查

```bash
# 检查端口
ss -tlnp | grep 8181

# 手动运行测试
cd /DATA/Documents/mdict-server && ./mdict-rs

# 检查词典文件
ls -la /DATA/Documents/mdict-server/mdict/
```
