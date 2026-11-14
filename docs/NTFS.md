# NTFS 写入组件与验证

2026-11-14：选择 NTFS-3G 2026.9.28 的用户态库，作为独立可选工作进程；Rust 核心、CLI 与 Windows GUI 共用这一写入路径。读取仍使用原生 Rust 适配器。

## 组件评估

| 组件 | 评估与决定 |
| --- | --- |
| fstool 0.5.0 / 0.5.4 | 原型对独立 mkntfs 样本的覆盖写入拒绝 `$LogFile` 布局。保留读取能力，不绕过检查启用它的 NTFS 写入器。旧探针证据保留在 ntfs-candidate-*.json。 |
| libfsntfs | 上游列出的 NTFS 支持是只读，不满足写入要求，未进行写入构建。见[上游能力说明](https://github.com/libyal/libfsntfs)。 |
| libguestfs | appliance 路线适合隔离和丰富的文件系统操作，但增加客体系统与部署依赖。当前优先 Windows 原生、无挂载后端，因此本阶段不采用；这是架构取舍。见[官方 FAQ](https://libguestfs.org/guestfs-faq.1.html)。 |
| NTFS-3G | 成熟的用户态读写实现，并提供自定义块设备回调。已构建 Linux 和 Windows x64 工作进程，复用同一分区窗口和 QCOW2 事务。见[上游项目](https://github.com/tuxera/ntfs-3g)。 |

最终固定到 2026.9.28，采用上游发布记录中的 SHA-256 校验值。该稳定版本包含此前的索引、ACL 和属性解析修复，以及后续回归修复；未将发行版预装的旧库作为产品写入依赖。依据：[上游发布记录](https://github.com/tuxera/ntfs-3g/releases/tag/2026.9.28)。

## 写入与提交

1. 父进程创建本次操作专用的 QCOW2 子覆盖层。
2. 工作进程通过管道请求分区内的读、写、刷新和上传内容；父进程校验范围和整数溢出，每帧最多 1 MiB。工作进程不接收宿主镜像路径，禁用库的默认宿主设备 I/O。
3. 先以只读 forensic 模式检查卷状态和休眠文件，再在不传入 force/recover/ignore-hibernation 的情况下打开写入。遵循 NTFS-3G 自身的干净卷处理流程，不实现日志修复。
4. 覆盖普通文件只修改其原有 unnamed DATA 属性。新文件继承 NTFS 父目录的安全描述符；不使用宿主 Windows ACL 或 Linux 账号替代来宾权限。
5. 完成库卸载、NBD FLUSH、覆盖层刷盘和检查、源指纹复核后才更新 manifest。错误或工作进程退出会丢弃本次子层。

新建文件后先关闭并同步父目录索引，再同步子文件，避免库通过独立句柄更新 FILE_NAME 时读到尚未落盘的父目录状态。改名先同步目标索引，再重新打开源目录完成删除旧链接。

## 可复现验证

```bash
python3 scripts/build_ntfs_worker.py
cargo build --locked --release
python3 scripts/ntfs_validate.py --directory artifacts/ntfs-linux --output artifacts/ntfs-linux-results.json --large-volume
python3 scripts/build_ntfs_worker.py --target windows
```

Windows 产物可在 WSL 中驱动，实际运行的是 Windows xetch、QEMU 和工作进程：

```bash
python3 scripts/ntfs_validate.py --binary target/windows/release/xetch.exe --directory artifacts/ntfs-windows --output artifacts/ntfs-windows-results.json --large-volume
```

需先完成 Windows Rust 构建和项目内 QEMU 准备。独立检查使用系统 mkntfs、ntfscp、ntfscat、ntfsinfo、ntfsfix，不挂载镜像。目录必须不存在，所有输入均为临时测试样本。运行证据：[Linux](ntfs-linux-results.json)、[Windows 原生](ntfs-windows-results.json)。

覆盖：驻留/非驻留/空文件转换、原文件替换、中文和非 BMP 名称、新建目录、文件移动、删除空目录、IMG 与 VMDK 源及导出。通过独立工具逐字节比较原文件 ACL、安全描述符、ADS、EA，并检查 MFT 编号不变。另检查输出一致性、输出文件内容、源 SHA-256、空间不足回退、目标冲突、非空目录拒绝、系统路径保护、脏卷/休眠卷/损坏日志拒绝和工作进程在写入一个扇区后退出的回退。`--large-volume` 另测 5 GiB NTFS 卷及位于高偏移的 MFT 镜像；不代表已验证超过 4 GiB 的单个文件。最终 Linux、Windows 各通过 14 项检查。

## 支持边界

- 已验证正常关闭的 NTFS 3.1 离线样本。支持普通文件导入/替换、文件夹创建、文件/空目录删除、文件改名或移动。
- 拒绝 NTFS 元数据路径、目录改名、压缩/EFS/reparse 文件的修改。未开放 ADS、EA、ACL 编辑和 NTFS 格式化。
- 未验证所有 Windows/VMware 来源镜像、目录命名空间和复杂硬链接组合、Windows chkdsk、恶意输入隔离、真实断电或宿主磁盘故障。`ntfsfix -n` 不能等同于 chkdsk 认证。
- 没有可用且版本匹配的工作进程时，NTFS 自动保持只读；实验开关不会绕过这一门槛。

Windows 移植仅补齐缺失的 POSIX 类型/模式、账号查询和 CRT 随机源，保留 GCC 属性，并强制 64 位偏移。未改写 NTFS 布局、日志和权限算法。工作进程是一条进程边界，不是操作系统安全沙箱。

## 构建与分发材料

构建脚本固定源包 URL 与 SHA-256，生成 `ntfs-worker-profile.json`，保留构建参数和工作进程哈希。NTFS-3G 与本工作进程使用 GPL-2.0-or-later；Rust 应用的 Apache-2.0 许可证保持独立。许可证、对应源包、本项目的适配文件和构建脚本随工作进程构建材料提供。协议与构建细节见 [native/ntfs-worker/README.md](../native/ntfs-worker/README.md)。完整发布打包及签名仍待完成。
