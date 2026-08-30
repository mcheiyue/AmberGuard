#!/system/bin/sh
# late_start: binary ONLY under module dir (NOT system/bin — Magisk mounts that to /system)

MODDIR=${0%/*}
AG_DIR=/data/adb/amberguard
LOG_DIR=$AG_DIR/log
LOG=$AG_DIR/service.log
UPDATE_DIR=/data/adb/modules_update/AmberGuard
HOT_READY=$AG_DIR/hot_update_ready
HOT_FAILED=$AG_DIR/hot_update_failed
ACTIVE_DAEMON=""
for c in "$MODDIR/bin/amberguard" "$MODDIR/amberguard" "$MODDIR/system/bin/amberguard"; do
  if [ -f "$c" ]; then
    ACTIVE_DAEMON=$c
    break
  fi
done
UPDATE_DAEMON=$UPDATE_DIR/bin/amberguard
MAX_CRASH=5
MAX_SLEEP=30

mkdir -p "$AG_DIR" "$LOG_DIR"

if [ -f "$LOG" ]; then
  sz=$(wc -c <"$LOG" 2>/dev/null || echo 0)
  if [ "$sz" -gt 1048576 ] 2>/dev/null; then
    mv -f "$LOG" "$LOG.1" 2>/dev/null
  fi
fi

if [ ! -f "$UPDATE_DAEMON" ]; then
  rm -f "$HOT_READY" "$HOT_FAILED"
fi

candidate_version() {
  sed -n 's/^version=//p' "$UPDATE_DIR/module.prop" 2>/dev/null | head -n 1
}

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
  echo "MODDIR=$MODDIR ACTIVE_DAEMON=$ACTIVE_DAEMON"
  wait_boot

  if [ -z "$ACTIVE_DAEMON" ] && [ ! -f "$UPDATE_DAEMON" ]; then
    echo "daemon missing under active and update module dirs, exit"
    exit 0
  fi
  [ -n "$ACTIVE_DAEMON" ] && chmod 755 "$ACTIVE_DAEMON" 2>/dev/null

  export RUST_LOG="${RUST_LOG:-info}"
  export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

  crash=0
  sleep_s=5
  use_update=0
  while true; do
    DAEMON=$ACTIVE_DAEMON
    DAEMON_SOURCE=active
    ready_source=$(cat "$HOT_READY" 2>/dev/null | head -n 1)
    candidate_ver=$(candidate_version)
    failed_version=$(cat "$HOT_FAILED" 2>/dev/null | head -n 1)
    if [ "$use_update" = 1 ] || [ "$ready_source" = "modules_update" ]; then
      if [ -f "$UPDATE_DAEMON" ] && [ "$candidate_ver" != "$failed_version" ]; then
        DAEMON=$UPDATE_DAEMON
        DAEMON_SOURCE=modules_update
        use_update=1
      else
        use_update=0
        rm -f "$HOT_READY"
      fi
    fi
    if [ -z "$DAEMON" ] || [ ! -f "$DAEMON" ]; then
      echo "daemon missing under active and update module dirs, exit"
      exit 0
    fi
    chmod 755 "$DAEMON" 2>/dev/null
    echo "start $DAEMON source=$DAEMON_SOURCE crash=$crash"
    setsid "$DAEMON" >>"$LOG" 2>&1
    rc=$?
    if [ "$DAEMON_SOURCE" = modules_update ] && [ $rc -ne 0 ]; then
      failed_version=$(candidate_version)
      printf '%s\n' "$failed_version" >"$HOT_FAILED"
      rm -f "$HOT_READY"
      use_update=0
      echo "update candidate failed rc=$rc version=$failed_version, fallback active"
    elif [ "$DAEMON_SOURCE" = modules_update ]; then
      use_update=1
    fi
    # 只有真正崩溃（rc!=0）才计入；热更新 exit(0) 属正常退出，清零避免累计到上限后停拉起
    if [ $rc -ne 0 ]; then
        crash=$((crash + 1))
    else
        crash=0
    fi
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
