#!/system/bin/sh
# post-fs-data：只做数据目录准备。SELinux 交给 Magisk 加载 sepolicy.rule
# （不再 magiskpolicy --live，避免重复注入与策略噪音）

MODDIR=${0%/*}
AG_DIR=/data/adb/amberguard
LOG=$AG_DIR/post-fs-data.log

mkdir -p "$AG_DIR/log"
{
  echo "=== AmberGuard post-fs-data $(date) ==="
  echo "MODDIR=$MODDIR"
  echo "sepolicy: Magisk 自动加载 sepolicy.rule（本脚本不 --live）"
  echo "post-fs-data 完成"
} >>"$LOG" 2>&1
