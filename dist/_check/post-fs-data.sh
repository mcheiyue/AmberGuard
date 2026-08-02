#!/system/bin/sh
# post-fs-data：阻塞阶段（Zygote 前）注入 SELinux，杜绝时序竞态
# 仅做 magiskpolicy --live，不启动 daemon

MODDIR=${0%/*}
AG_DIR=/data/adb/amberguard
LOG=$AG_DIR/post-fs-data.log

mkdir -p "$AG_DIR"
{
  echo "=== AmberGuard post-fs-data $(date) ==="

  # PLAN §7.3 显式 allow（不展开 AOSP 宏）；与 sepolicy.rule 保持一致
  # 类型名以目标机实测为准，以下为起点草稿
  magiskpolicy --live \
    "allow magisk wpa_socket:sock_file { read write getattr open }" \
    "allow magisk wpa_socket:dir { search getattr }" \
    "allow magisk wifi_data_file:dir { search getattr }" \
    "allow magisk wifi_data_file:sock_file { create read write getattr open unlink }" \
    "allow magisk netlink_generic_socket:socket { create bind read write getattr setsockopt }" \
    "allow magisk netlink_socket:socket { create bind read write getattr setsockopt }" \
    "allow magisk port_t:tcp_socket { name_bind read write getattr setopt }" \
    "allow magisk node_t:tcp_socket { node_bind read write }" \
    "allow magisk sysfs_leds:dir { search getattr }" \
    "allow magisk sysfs_leds:file { read getattr open }" \
    "allow magisk input_device:dir { search getattr }" \
    "allow magisk input_device:chr_file { read getattr open }" \
    "allow magisk proc_net:file { read getattr open }"

  rc=$?
  if [ $rc -eq 0 ]; then
    echo "magiskpolicy --live 注入成功"
  else
    echo "magiskpolicy --live 失败 rc=$rc（sepolicy.rule 静态打底仍可能生效）"
  fi

  # 将同套规则写入数据目录副本，便于实机对照；模块根 sepolicy.rule 为 Magisk 静态基线
  if [ -f "$MODDIR/sepolicy.rule" ]; then
    cp -f "$MODDIR/sepolicy.rule" "$AG_DIR/sepolicy.rule.baseline" 2>/dev/null
    echo "已同步 sepolicy.rule 基线副本到 $AG_DIR/sepolicy.rule.baseline"
  fi

  echo "post-fs-data 完成"
} >>"$LOG" 2>&1
