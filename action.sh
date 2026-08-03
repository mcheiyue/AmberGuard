#!/system/bin/sh
# Magisk v28+ / 部分 KSU：点「操作」执行
# - 输出必须走 STDOUT（echo），ui_print 仅安装期可用 → 之前「操作失败」根因
# - 结束后管理器会重读 module.prop，可在此刷新 description

MODDIR=${0%/*}
AG=/data/adb/amberguard
STATUS=$AG/status.txt
PROP="$MODDIR/module.prop"

echo "—— AmberGuard 状态 ——"

RUNNING=0
if pidof amberguard >/dev/null 2>&1; then
  RUNNING=1
elif pgrep amberguard >/dev/null 2>&1; then
  RUNNING=1
fi

if [ "$RUNNING" = 1 ]; then
  echo "进程: 运行中"
else
  echo "进程: 未发现（可重启模块或看 service.log）"
fi

LINE=""
if [ -f "$STATUS" ]; then
  LINE=$(cat "$STATUS" 2>/dev/null | tr -d '\r' | head -n 1)
  echo "$LINE"
else
  echo "status.txt 尚无（daemon 未写或未启动）"
  LINE="daemon 无状态"
fi

if [ -f "$AG/config.toml" ]; then
  echo "配置: 已落盘"
else
  echo "配置: 未落盘（请开 WebUI 初始化）"
fi

echo "面板: http://127.0.0.1:8080/"
echo "日志: $AG/log/amberguard.log"
echo "仓库: https://github.com/mcheiyue/AmberGuard"

# 刷新列表 description（单行；Magisk 操作结束后会重读）
if [ -f "$PROP" ]; then
  # 取 status 前 ~80 字，去掉危险字符
  DESC=$(printf '%s' "$LINE" | tr '\n\r|#' '   ' | cut -c1-90)
  [ -z "$DESC" ] && DESC="AmberGuard"
  # 无 sed -i 兼容性差时用临时文件
  if grep -q '^description=' "$PROP" 2>/dev/null; then
    {
      grep -v '^description=' "$PROP"
      echo "description=$DESC"
    } >"$PROP.tmp" && mv -f "$PROP.tmp" "$PROP"
    echo "已刷新模块列表描述"
  fi
fi

echo "完成"
