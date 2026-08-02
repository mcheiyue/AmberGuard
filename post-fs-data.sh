#!/system/bin/sh
# post-fs-data：阻塞阶段注入 SELinux（Magisk 空格语法，非 type:class）

MODDIR=${0%/*}
AG_DIR=/data/adb/amberguard
LOG=$AG_DIR/post-fs-data.log
RULE=$MODDIR/sepolicy.rule

mkdir -p "$AG_DIR"
{
  echo "=== AmberGuard post-fs-data $(date) ==="

  if [ -f "$RULE" ]; then
    # 逐行 --live，跳过空行与注释
    while IFS= read -r line || [ -n "$line" ]; do
      case "$line" in
        ''|\#*) continue ;;
      esac
      echo "apply: $line"
      magiskpolicy --live "$line" 2>&1 || echo "WARN: failed: $line"
    done < "$RULE"
    cp -f "$RULE" "$AG_DIR/sepolicy.rule.baseline" 2>/dev/null
    echo "已同步 sepolicy.rule → $AG_DIR/sepolicy.rule.baseline"
  else
    echo "ERROR: missing $RULE"
  fi

  echo "post-fs-data 完成"
} >>"$LOG" 2>&1
