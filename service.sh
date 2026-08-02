#!/system/bin/sh
# service.sh：late_start 非阻塞阶段，只启动 daemon，不碰 SELinux
# Phase 1 骨架：二进制可能尚不存在，缺失时打日志退出

MODDIR=${0%/*}
AG_DIR=/data/adb/amberguard
LOG=$AG_DIR/service.log
# 占位路径：后续 cargo-ndk 产物落到 system/bin/amberguard
DAEMON=$MODDIR/system/bin/amberguard
# 连续崩溃上限（S13）：防死循环耗电
MAX_CRASH=5
# 崩溃间隔退避上限（秒）
MAX_SLEEP=30

mkdir -p "$AG_DIR"
{
  echo "=== AmberGuard service $(date) ==="
  echo "MODDIR=$MODDIR"

  if [ ! -x "$DAEMON" ] && [ ! -f "$DAEMON" ]; then
    echo "daemon 不存在: $DAEMON（Phase 1 骨架占位，跳过启动）"
    exit 0
  fi

  # 确保可执行
  chmod 755 "$DAEMON" 2>/dev/null

  # 详细日志进 service.log，方便实机排查 wpa 连接
  export RUST_LOG="${RUST_LOG:-info}"
  export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

  crash=0
  sleep_s=5
  while true; do
    echo "启动 daemon: $DAEMON (crash=$crash sleep=${sleep_s}s) RUST_LOG=$RUST_LOG"
    # setsid 脱离会话，避免 Magisk 脚本退出带走进程
    setsid "$DAEMON" >>"$LOG" 2>&1
    rc=$?
    crash=$((crash + 1))
    echo "daemon 退出 rc=$rc 连续崩溃=$crash/$MAX_CRASH"

    if [ $crash -ge $MAX_CRASH ]; then
      echo "连续崩溃 $MAX_CRASH 次，停止重启"
      break
    fi

    sleep $sleep_s
    # 简单退避，上限 MAX_SLEEP
    if [ $sleep_s -lt $MAX_SLEEP ]; then
      sleep_s=$((sleep_s + 5))
      [ $sleep_s -gt $MAX_SLEEP ] && sleep_s=$MAX_SLEEP
    fi
  done
} >>"$LOG" 2>&1
