#!/system/bin/sh
# late_start: binary ONLY under module dir (NOT system/bin — Magisk mounts that to /system)

MODDIR=${0%/*}
AG_DIR=/data/adb/amberguard
LOG_DIR=$AG_DIR/log
LOG=$AG_DIR/service.log
DAEMON=""
for c in "$MODDIR/bin/amberguard" "$MODDIR/amberguard" "$MODDIR/system/bin/amberguard"; do
  if [ -f "$c" ]; then
    DAEMON=$c
    break
  fi
done
MAX_CRASH=5
MAX_SLEEP=30

mkdir -p "$AG_DIR" "$LOG_DIR"

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
  echo "MODDIR=$MODDIR DAEMON=$DAEMON"
  wait_boot

  if [ -z "$DAEMON" ] || [ ! -f "$DAEMON" ]; then
    echo "daemon missing under module dir, exit"
    exit 0
  fi
  chmod 755 "$DAEMON" 2>/dev/null

  export RUST_LOG="${RUST_LOG:-info}"
  export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

  crash=0
  sleep_s=5
  while true; do
    echo "start $DAEMON crash=$crash"
    setsid "$DAEMON" >>"$LOG" 2>&1
    rc=$?
    crash=$((crash + 1))
    echo "exit rc=$rc ($crash/$MAX_CRASH)"
    if [ $crash -ge $MAX_CRASH ]; then
      echo "crash limit, stop"
      break
    fi
    sleep $sleep_s
    if [ $sleep_s -lt $MAX_SLEEP ]; then
      sleep_s=$((sleep_s + 5))
      [ $sleep_s -gt $MAX_SLEEP ] && sleep_s=$MAX_SLEEP
    fi
  done
} >>"$LOG" 2>&1