#!/system/bin/sh
# 安装时：探测网卡，仅在无配置时写默认 config.toml

AG_DIR=/data/adb/amberguard
CONFIG=$AG_DIR/config.toml

ui_print "- AmberGuard"

mkdir -p "$AG_DIR/log"

WLAN=wlan0
if [ -d /sys/class/net/wlan0 ]; then
  WLAN=wlan0
elif [ -d /sys/class/net ]; then
  for iface in /sys/class/net/*; do
    name=${iface##*/}
    case "$name" in
      wlan*) WLAN=$name; break ;;
    esac
  done
fi
ui_print "- interface: $WLAN"

if [ ! -f "$CONFIG" ]; then
  cat >"$CONFIG" <<EOF
interface = "$WLAN"
listen = "127.0.0.1:8080"
upswitch_rssi_min_dbm = -65
score_detect_threshold = 70.0
score_switch_threshold = 30.0
mode = "daily"
log_level = "info"
EOF
  ui_print "- wrote $CONFIG"
else
  ui_print "- keep existing config"
fi

ui_print "- done (no system.prop fingerprint)"
