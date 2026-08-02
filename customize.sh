#!/system/bin/sh
# customize.sh：Magisk 安装时执行，检测网卡并写默认配置 stub
# SKIPUNZIP=0 时由 Magisk 解压后调用；ui_print 由 Magisk 注入

AG_DIR=/data/adb/amberguard
CONFIG=$AG_DIR/config.toml

ui_print "- AmberGuard 安装配置"

mkdir -p "$AG_DIR"

# 探测 wlan 接口：优先 wlan0，其次 /sys/class/net 下含 wlan 的接口
WLAN=wlan0
if [ -d /sys/class/net/wlan0 ]; then
  WLAN=wlan0
elif [ -d /sys/class/net ]; then
  for iface in /sys/class/net/*; do
    name=${iface##*/}
    case "$name" in
      wlan*|wlan)
        WLAN=$name
        break
        ;;
    esac
  done
fi

ui_print "- 检测到无线接口: $WLAN"

# 仅在不存在时写默认 stub，避免覆盖用户已有配置
if [ ! -f "$CONFIG" ]; then
  cat >"$CONFIG" <<EOF
# AmberGuard 默认配置（Phase 1 骨架 stub）
# 后续 daemon 读取此文件

interface = "$WLAN"
# Web 面板仅本机
web_bind = "127.0.0.1:8080"
# 首选频段：5g / 24g
preferred_band = "5g"
# 上切对侧 RSSI 下限（dBm）
upswitch_rssi_min_dbm = -65
# 工作模式：daily / power_save / pause
mode = "daily"
EOF
  ui_print "- 已创建 $CONFIG"
else
  ui_print "- 保留已有配置 $CONFIG"
fi

mkdir -p "$AG_DIR/log"

ui_print " "
ui_print "- 说明："
ui_print "  · Web 面板: http://127.0.0.1:8080 （KSU WebUI 可从模块入口打开）"
ui_print "  · 日志: $AG_DIR/log/amberguard.log"
ui_print "  · 配置: $CONFIG"
ui_print " "
