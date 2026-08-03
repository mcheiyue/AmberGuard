#!/system/bin/sh
# Magisk「操作」：必须用 echo；刷新中文 description（模块实时状态）

MODDIR=${0%/*}
AG=/data/adb/amberguard
STATUS=$AG/status.txt
PROP="$MODDIR/module.prop"

echo "—— AmberGuard 模块状态 ——"

if pidof amberguard >/dev/null 2>&1 || pgrep amberguard >/dev/null 2>&1; then
  echo "进程：运行中"
else
  echo "进程：未运行（可重启模块或查看 service.log）"
fi

LINE=""
if [ -f "$STATUS" ]; then
  LINE=$(cat "$STATUS" 2>/dev/null | tr -d '\r' | head -n 1)
  echo "状态：$LINE"
else
  echo "状态：尚无（守护进程未写入）"
  LINE="AmberGuard · 等待守护进程"
fi

if [ -f "$AG/config.toml" ]; then
  echo "配置：已保存"
else
  echo "配置：未保存（请打开面板初始化）"
fi

echo "面板：http://127.0.0.1:8080/"
echo "日志：$AG/log/amberguard.log"
echo "仓库：https://github.com/mcheiyue/AmberGuard"

if [ -f "$PROP" ]; then
  DESC=$(printf '%s' "$LINE" | tr '\n\r#=' '    ' | cut -c1-96)
  [ -z "$DESC" ] && DESC="AmberGuard · 双频守护"
  {
    grep -v '^description=' "$PROP" 2>/dev/null
    echo "description=$DESC"
  } >"$PROP.tmp" && mv -f "$PROP.tmp" "$PROP"
  echo "已更新模块列表描述"
fi

echo "完成"
