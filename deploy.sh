#!/bin/bash
# MDict 词典 Armbian 部署脚本
# 使用方法: ./deploy.sh user@armbian-ip [部署目录]
# 示例: ./deploy.sh root@192.168.1.100 /DATA/Documents

set -e

REMOTE="${1:-user@armbian}"
BASE_DIR="${2:-/DATA/Documents}"  # 基础目录
APP_DIR="$BASE_DIR/mdict-server"   # 应用目录
LOCAL_BIN="target/aarch64-unknown-linux-musl/release/mdict-rs"
LOCAL_STATIC="resources/static"

echo "🚀 MDict 词典部署脚本"
echo "===================="

# 检查参数
if [ "$1" == "-h" ] || [ "$1" == "--help" ]; then
    echo "使用方法: ./deploy.sh user@armbian-ip [基础目录]"
    echo ""
    echo "参数:"
    echo "  user@armbian-ip  SSH 连接地址 (必填)"
    echo "  基础目录          远程基础路径 (默认: /DATA/Documents)"
    echo ""
    echo "部署后目录结构:"
    echo "  基础目录/mdict-server/"
    echo "  ├── mdict-rs     # 二进制"
    echo "  ├── static/      # 静态资源"
    echo "  └── mdict/       # 词典文件"
    echo ""
    echo "示例:"
    echo "  ./deploy.sh root@192.168.1.100"
    echo "  ./deploy.sh root@192.168.1.100 /opt"
    exit 0
fi

# 检查二进制是否存在
if [ ! -f "$LOCAL_BIN" ]; then
    echo "❌ 未找到 ARM64 二进制文件，正在编译..."
    cargo build --release --target aarch64-unknown-linux-musl
fi

echo "📦 二进制文件大小: $(ls -lh $LOCAL_BIN | awk '{print $5}')"
echo "📡 目标服务器: $REMOTE"
echo "📁 部署目录: $APP_DIR"
echo ""

# 创建远程目录
echo "1️⃣ 创建远程目录..."
ssh "$REMOTE" "mkdir -p $APP_DIR/mdict $APP_DIR/static"

# 复制二进制
echo "2️⃣ 上传二进制文件..."
scp "$LOCAL_BIN" "$REMOTE:$APP_DIR/"

# 复制静态资源
echo "3️⃣ 上传静态资源..."
scp -r "$LOCAL_STATIC/"* "$REMOTE:$APP_DIR/static/"

# 设置权限
echo "4️⃣ 设置执行权限..."
ssh "$REMOTE" "chmod +x $APP_DIR/mdict-rs"

# 创建 systemd 服务
echo "5️⃣ 配置 systemd 服务..."
ssh "$REMOTE" "sudo tee /etc/systemd/system/mdict.service > /dev/null << EOF
[Unit]
Description=MDict Dictionary Server
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=$APP_DIR
ExecStart=$APP_DIR/mdict-rs
Restart=always
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF"

# 重载并启动服务
echo "6️⃣ 启动服务..."
ssh "$REMOTE" "sudo systemctl daemon-reload && sudo systemctl enable mdict && sudo systemctl restart mdict"

# 等待服务启动
sleep 2

# 检查状态
echo ""
echo "✅ 部署完成!"
echo ""
ssh "$REMOTE" "sudo systemctl status mdict --no-pager" || true

echo ""
echo "📁 目录结构:"
echo "   $APP_DIR/"
echo "   ├── mdict-rs     # 二进制"
echo "   ├── static/      # 静态资源"
echo "   └── mdict/       # 词典文件 ← 把 .mdx 和 .mdd 放这里"
echo ""
echo "🌐 访问地址: http://$(echo $REMOTE | cut -d@ -f2):8181"
echo ""
echo "📚 上传词典文件:"
echo "   scp 词典.mdx 词典.mdd $REMOTE:$APP_DIR/mdict/"
echo ""
echo "🔄 重启服务生效:"
echo "   ssh $REMOTE 'sudo systemctl restart mdict'"
echo ""
echo "常用命令:"
echo "  查看日志: ssh $REMOTE 'journalctl -u mdict -f'"
echo "  重启服务: ssh $REMOTE 'sudo systemctl restart mdict'"
echo "  停止服务: ssh $REMOTE 'sudo systemctl stop mdict'"
