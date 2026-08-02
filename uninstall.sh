#!/system/bin/sh
# Magisk 卸载时调用：清数据目录，避免残留指纹/日志
AG_DIR=/data/adb/amberguard

# 停掉可能还在跑的守护（若已 disable 通常已死）
killall -9 amberguard 2>/dev/null

rm -rf "$AG_DIR" 2>/dev/null

# 不碰 sepolicy 运行时状态——重启后 Magisk 会卸掉本模块规则
exit 0
