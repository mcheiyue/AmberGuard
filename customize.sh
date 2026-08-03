#!/system/bin/sh
# 安装时：探测网卡；无配置则写默认；有配置则音量键选保留/重置（超时=保留）

AG_DIR=/data/adb/amberguard
CONFIG=$AG_DIR/config.toml
LOG_DIR=$AG_DIR/log

ui_print "- AmberGuard"

mkdir -p "$AG_DIR" "$LOG_DIR"

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

write_default_config() {
  cat >"$CONFIG" <<EOF
interface = "$WLAN"
listen = "127.0.0.1:8080"
upswitch_rssi_min_dbm = -65
score_detect_threshold = 70.0
score_switch_threshold = 30.0
mode = "daily"
log_level = "info"
EOF
}

# 音量键：0=保留(+)  1=重置(-)  超时/失败=0
# ponytail: 无 getevent/timeout 则直接保留，不半残卡死安装
vk_keep_or_reset() {
  local wait_s=8
  ui_print " "
  ui_print "- 检测到已有配置 $CONFIG"
  ui_print "- 音量加(+)：保留配置并更新模块（推荐）"
  ui_print "- 音量减(-)：备份后写入日用默认"
  ui_print "- ${wait_s}s 无按键 → 保留"
  ui_print " "

  GETEVENT=""
  if command -v getevent >/dev/null 2>&1; then
    GETEVENT=$(command -v getevent)
  elif [ -x /system/bin/getevent ]; then
    GETEVENT=/system/bin/getevent
  elif [ -x /bin/getevent ]; then
    GETEVENT=/bin/getevent
  fi

  if [ -z "$GETEVENT" ]; then
    ui_print "- 无 getevent，默认保留配置"
    return 0
  fi

  # 有 timeout 用短轮询；没有则 sleep 循环 + 后台 getevent
  local has_timeout=0
  if command -v timeout >/dev/null 2>&1; then
    has_timeout=1
  fi

  local end t line
  end=$(( $(date +%s) + wait_s ))
  while [ "$(date +%s)" -lt "$end" ]; do
    line=""
    if [ "$has_timeout" = 1 ]; then
      line=$(timeout 1 "$GETEVENT" -qlc 1 2>/dev/null | grep KEY_VOLUME | head -n 1)
    else
      # 后台读一行，1s 后杀掉
      line=$("$GETEVENT" -qlc 1 2>/dev/null | grep KEY_VOLUME | head -n 1 &)
      sleep 1
    fi
    case "$line" in
      *KEY_VOLUMEUP*|*VOLUME_UP*)
        ui_print "- 已选：保留配置"
        return 0
        ;;
      *KEY_VOLUMEDOWN*|*VOLUME_DOWN*)
        ui_print "- 已选：重置配置"
        return 1
        ;;
    esac
  done
  ui_print "- 超时，保留配置"
  return 0
}

if [ ! -f "$CONFIG" ]; then
  write_default_config
  ui_print "- wrote $CONFIG"
else
  if vk_keep_or_reset; then
    ui_print "- keep existing config"
  else
    ts=$(date +%Y%m%d%H%M%S 2>/dev/null || echo old)
    bak="${CONFIG}.bak.${ts}"
    cp -f "$CONFIG" "$bak" 2>/dev/null && ui_print "- backup → $bak"
    write_default_config
    ui_print "- reset config (defaults, interface=$WLAN)"
  fi
fi

ui_print "- done (no system.prop fingerprint)"
