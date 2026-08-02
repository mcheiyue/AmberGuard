#!/system/bin/sh
# late_start：等开机完成后再起 daemon（参考 FreePPS wait_until_login）

MODDIR=${0%/*}
AG_DIR=/data/adb/amberguard
LOG_DIR=$AG_DIR/log
LOG=$AG_DIR/service.log
DAEMON=$MODDIR/system/bin/amberguard
MAX_CRASH=5
MAX_SLEEP=30

mkdir -p "$AG_DIR" "$LOG_DIR"

# service.log 轮转
if [ -f "$LOG" ]; then
  sz=$(wc -c <"$LOG" 2>/dev/null || echo 0)
  if [ "$sz" -gt 1048576 ] 2>/dev/null; then
    mv -f "$LOG" "$LOG.1" 2>/dev/null
  fi
fi

wait_boot() {
  while [ "$(getprop sys.boot_completed)" != "1" ]; do
    sleep 1
  done
  # 用户解锁前 /data 可能未就绪；短等即可，不强制写 sdcard
  local i=0
  while [ $i -lt 30 ]; do
    [ -d /data/adb ] && break
    sleep 1
    i=$((i + 1))
  done
  sleep 3
}

{
  echo "=== AmberGuard service $(date) ==="
  echo "MODDIR=$MODDIR"
  wait_boot
  echo "boot_completed, start daemon"

  if [ ! -f "$DAEMON" ]; then
    echo "daemon 不存在: $DAEMON，退出"
    exit 0
  fi
  chmod 755 "$DAEMON" 2>/dev/null

  export RUST_LOG="${RUST_LOG:-info}"
  export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

  crash=0
  sleep_s=5
  while true; do
    echo "启动 daemon crash=$crash sleep=${sleep_s}s"
    # setsid 脱离会话
    setsid "$DAEMON" >>"$LOG" 2>&1
    rc=$?
    crash=$((crash + 1))
    echo "daemon 退出 rc=$rc ($crash/$MAX_CRASH)"
    if [ $crash -ge $MAX_CRASH ]; then
      echo "连续崩溃达上限，停止"
      break
    fi
    sleep $sleep_s
    if [ $sleep_s -lt $MAX_SLEEP ]; then
      sleep_s=$((sleep_s + 5))
      [ $sleep_s -gt $MAX_SLEEP ] && sleep_s=$MAX_SLEEP
    fi
  done
} >>"$LOG" 2>&1
