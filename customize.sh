#!/system/bin/sh
# 安装：建目录；首次不预写 config（交给面板初始化）
# 已有配置：音量+保留 / 音量-备份后删除（像新装）/ 超时保留

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
# 供面板/文档参考；不强制写进 config
echo "$WLAN" >"$AG_DIR/iface.guess" 2>/dev/null

# 音量键：0=保留  1=重置(删配置)  超时/失败=0
vk_keep_or_reset() {
  local wait_s=8
  ui_print " "
  ui_print "- 检测到已有配置 $CONFIG"
  ui_print "- 音量加(+)：保留配置并更新模块（推荐）"
  ui_print "- 音量减(-)：备份后删除配置（等同新装，进面板初始化）"
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

  local has_timeout=0
  if command -v timeout >/dev/null 2>&1; then
    has_timeout=1
  fi

  local end line
  end=$(( $(date +%s) + wait_s ))
  while [ "$(date +%s)" -lt "$end" ]; do
    line=""
    if [ "$has_timeout" = 1 ]; then
      line=$(timeout 1 "$GETEVENT" -qlc 1 2>/dev/null | grep KEY_VOLUME | head -n 1)
    else
      line=$("$GETEVENT" -qlc 1 2>/dev/null | grep KEY_VOLUME | head -n 1 &)
      sleep 1
    fi
    case "$line" in
      *KEY_VOLUMEUP*|*VOLUME_UP*)
        ui_print "- 已选：保留配置"
        return 0
        ;;
      *KEY_VOLUMEDOWN*|*VOLUME_DOWN*)
        ui_print "- 已选：重置（删除配置）"
        return 1
        ;;
    esac
  done
  ui_print "- 超时，保留配置"
  return 0
}

if [ ! -f "$CONFIG" ]; then
  ui_print "- 无配置文件：不预写，首次请开 WebUI 初始化"
else
  if vk_keep_or_reset; then
    ui_print "- keep existing config"
  else
    ts=$(date +%Y%m%d%H%M%S 2>/dev/null || echo old)
    bak="${CONFIG}.bak.${ts}"
    cp -f "$CONFIG" "$bak" 2>/dev/null && ui_print "- backup → $bak"
    rm -f "$CONFIG"
    ui_print "- removed config（下次启动等同新装引导）"
    # 只保留最近 3 份 bak，避免堆积
    ls -1t "$AG_DIR"/config.toml.bak.* 2>/dev/null | tail -n +4 | while read -r old; do
      rm -f "$old"
    done
  fi
fi

ui_print "- done (no system.prop fingerprint)"
