# 低指纹说明

## 已确认问题（实机）

- 模块**启用** → 环境检测报 Root；**仅关闭**（不卸载）→ 不报。
- 说明触发源是「启用时生效」的挂载 / sepolicy / 进程，不是残留文件 alone。

## 主因

| 项 | 风险 | 处理 |
|----|------|------|
| 二进制在 `system/bin/amberguard` | Magisk 挂载到真实 `/system/bin/`，扫描器必查 | **v0.4.2 改为 `bin/amberguard`（模块目录内，参考 FreePPS）** |
| `ro.amberguard.*` system.prop | getprop 指纹 | v0.4.1 已去掉 |
| 无 uninstall.sh | 数据目录残留 | v0.4.1 已加 |
| sepolicy 扩展 magisk 域 | 可能干扰 Shamiko | 仍保留 wpa 最小 allow；若仍报再收敛 tcp |

## 参考

- FreePPS：`$MODDIR/bin/FreePPS`，不进 system/
- CleanZero：`$MODDIR/service`，不进 system/
