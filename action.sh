#!/system/bin/sh
# Magisk / KernelSU「操作」按钮：打印简要运行状态
# 详细面板：http://127.0.0.1:8080/ 或模块 WebUI

AG=/data/adb/amberguard
STATUS=$AG/status.txt

ui_print "—— AmberGuard 状态 ——"

if pgrep -f amberguard >/dev/null 2>&1 || pidof amberguard >/dev/null 2>&1; then
  ui_print "进程: 运行中"
else
  ui_print "进程: 未发现（可重启或看 service.log）"
fi

if [ -f "$STATUS" ]; then
  # 单行摘要
  ui_print "$(cat "$STATUS" 2>/dev/null | tr -d '\r' | head -n 1)"
else
  ui_print "status.txt 尚无（daemon 未写或未启动）"
fi

if [ -f "$AG/config.toml" ]; then
  ui_print "配置: 已落盘"
else
  ui_print "配置: 未落盘（请开 WebUI 初始化）"
fi

ui_print "面板: http://127.0.0.1:8080/"
ui_print "日志: $AG/log/amberguard.log"
ui_print "仓库: https://github.com/mcheiyue/AmberGuard"
