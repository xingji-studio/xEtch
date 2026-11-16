# exFAT / ext4 深度验收

这些测试使用真实文件系统驱动、完整大文件和真实 QEMU I/O 故障。
常规深度矩阵的后端在当前用户权限下运行，只提升 Linux 挂载辅助脚本的权限。
Windows 自带驱动的独立入口需要在管理员 PowerShell 中执行，其 Windows
后端子进程也继承该测试会话的权限；非管理员运行由常规 Windows 矩阵另行覆盖。
所有测试必须使用新的私有输出目录，输入镜像不会指向宿主块设备。

## 结果与范围

- [Linux 后端完整矩阵](deep-fs-linux-results.json)
- [Windows 原生后端完整矩阵](deep-fs-windows-results.json)
- [构建、测试及元数据回归结果](deep-fs-quality-results.json)：Linux 27 项、Windows 25 项测试通过，ignored 为 0；两平台 Clippy 与四轮 1 KiB/4 KiB 元数据矩阵通过。
- [Windows 自带 exFAT 驱动验收结果](deep-fs-windows-driver-results.json)：3 个场景、21 组检查通过，覆盖 Windows/Linux 后端的小镜像往返，以及 Windows 后端超过 4 GiB 的完整文件往返。8 次真实挂载、读写、卸载和只读 chkdsk 均通过；剩余临时 VHD 已确认卸载，管理员测试进程已退出。

每个后端有 6 个场景、51 组检查：系统往返 exFAT 8 / ext4 9，
大文件各 10，故障恢复各 7。各阶段通过独立 `fsck.exfat -n` 或
`e2fsck -fn`。系统往返与大文件结果中的 `driver` 明确记录实际
Linux 内核版本；“Windows 后端”表示磁盘操作由 Windows 原生程序完成。
ext4 的系统驱动复核由 Linux 完成。

大文件完整填充 4,297,064,585 字节（4 GiB + 2 MiB + 137），包含逐块
偏移标记；SHA-256 为
`de0b77b0fa8a6692120e8cfefac14f713495e24ea00559ecf2441d83238df2a9`。
分别执行新增、完整 VMDK 导出读回、缩小截断、覆盖为大文件、改名、
IMG 导出、内核完整读取及写入、后端再次完整读取。

ext4 系统场景还让实际内核回放后端日志：回滚测试副本的目录原位置块，
标记日志待回放，确认内核恢复目录改名。该测试不改变后端拒绝外来脏卷的策略。

故障代理仅为 Cargo example，正常构建/打包不会选用它。它把 QEMU blkdebug
接入私有 QCOW2 的 file 子节点，在写入已开始后注入 EIO、ENOSPC 或 FLUSH
失败；另在观测到私有层超过 2 MiB 后单独强制终止 QSD 或后端。每次检查：

- 输入镜像、旧 head 和 manifest 内容不变，旧文件完整可读。
- 记录中的未提交私有层被回收，无关的同名模式文件保留。
- QSD 和测试代理均已退出，继续编辑和导出成功。
- 提交已完成但清理记录尚存时，保留已提交的层。

覆盖范围是确定的进程终止/I/O 故障窗口；尚未模拟物理掉电、存储缓存丢失、
撕裂写入、任意指令边界终止、Windows 辅助进程归属建立前的启动竞态或 macOS。
CSUM_V3 和默认无校验和 JBD2 已有真实内核往返证据；后续新增 CSUM_V2、
2 KiB 块、提交边界及碎片化随机矩阵，见 [进阶验收](ADVANCED-FS-VALIDATION.md)。
更多复杂日志布局、旧内核持续写入 CSUM_V2 的原生样本仍需扩充。

## Linux 与 WSL

需要 Rust、Python 3.10+、QEMU、exfatprogs、e2fsprogs，以及内核 loop、
exFAT/ext4 支持。普通 Linux 使用 `sudo -n` 调用挂载辅助脚本，因此运行前
需具备可用的 sudo 凭据/策略。WSL 可显式指定发行版，以该发行版的 root
运行辅助脚本。只有该辅助脚本以提升权限运行，后端自身保持当前用户权限。

```bash
cargo build --locked --release --workspace
cargo build --locked --release -p xetch-core --example qsd_fault_proxy
python3 scripts/deep_filesystems_validate.py \
  --suite all --directory artifacts/deep-linux \
  --output artifacts/deep-linux-results.json

# WSL：辅助挂载进程使用指定发行版的 root。
python3 scripts/deep_filesystems_validate.py \
  --suite all --wsl-distribution Ubuntu-22.04 \
  --directory artifacts/deep-linux-wsl --output artifacts/deep-linux-wsl-results.json

# WSL：实际被测后端与 QEMU 均为 Windows 原生程序。
python3 scripts/deep_filesystems_validate.py \
  --binary target/windows/release/xetch.exe \
  --fault-proxy target/windows/release/examples/qsd_fault_proxy.exe \
  --qemu-directory artifacts/windows/qemu \
  --suite all --wsl-distribution Ubuntu-22.04 \
  --directory artifacts/deep-windows --output artifacts/deep-windows-results.json
```

Windows 故障代理同样通过原生 MSVC Cargo 构建，需设置开发者环境及项目
`CARGO_TARGET_DIR`。CI 的原生 Windows 常规编译会使用 `target/release`；
本地 Windows 构建默认使用 `target/windows/release`，按实际路径传参。

可用 `--suite system|large|fault` 单独运行，或通过 `--filesystems ext4`
限制文件系统。`--mkfs-exfat`、`--fsck-exfat` 支持指定工具路径。
`--payload` 可复用已有超过 4 GiB 的只读负载；报告记录实际大小与全量哈希。

单轮建议至少预留 27 GiB 可用空间，同时运行两套大文件矩阵建议预留
64 GiB。默认在单个大文件场景通过后删除其多 GiB 临时文件；失败保留材料，
`--keep-artifacts` 可保留成功样本。原始外部 `--payload` 不删除。
每轮复制并固定后端和代理副本，结果记录二进制 SHA-256、时间及驱动版本。

辅助脚本只接受带随机 token 标记目录内的普通镜像文件，在临时目录挂载，
使用 `nosuid,nodev,noexec`，在 finally 中卸载。卸载失败不会递归删除挂载点。

## Windows 自带 exFAT 驱动

在管理员 PowerShell 中运行；此处确实需要管理员权限，因为 Windows
附加 VHD 要求提升权限。入口不会自行更改 UAC 策略。

```powershell
./scripts/windows_exfat_interop_validate.ps1 `
  -WindowsBackend target/windows/release/xetch.exe `
  -QemuDirectory artifacts/windows/qemu `
  -Directory artifacts/native-windows-exfat `
  -Output artifacts/native-windows-exfat-results.json
```

可额外传入 `-LinuxBackend /mnt/c/.../target/release/xetch` 和
`-Distribution Ubuntu-22.04`，让同一 Windows 驱动矩阵也检查 Linux 后端。
该选项仅适用于已安装对应 WSL 发行版的开发机器；Windows CI 不依赖 WSL。
WSL 调用使用最少必要的 Windows 参数引号：该环境的 `wsl.exe` 会把带引号
的 `"-d"` 解释为 Linux 命令。预检已覆盖开关解析、双向路径转换和含空格路径
下复制后的 Linux 后端启动。其他工具仍采用默认的参数编码方式。

默认还执行原生 Windows 驱动的大文件往返：Windows 流式写入完整超过
4 GiB 的文件，后端完整读回、覆盖和改名，Windows 完整 SHA-256 读回并
写入反馈，最后由后端再次核对全部数据。可通过 `-LargePayload` 复用已有
4–5 GiB 的只读负载；未指定时生成与 Linux 矩阵相同的 4,297,064,585 字节
确定性数据。需要至少 27 GiB 空间，成功后清理该场景的多 GiB 文件。
`-SkipLargeFile` 仅运行小镜像往返；Windows CI 使用此选项控制磁盘占用，
超过 4 GiB 的 Windows 原生驱动验收在本地另行运行。

脚本只创建和附加私有目录内新的 128–256 MiB 或大文件测试的 6 GiB 固定 VHD，磁盘编号必须由
该确切镜像关联返回；格式化前要求该新磁盘为 RAW。只读 `chkdsk` 无 `/f`
或 `/r`，每次挂载都必须返回 0，日志与请求一起保留。所有成功附加的 VHD
都在 finally 中卸载。未接受物理磁盘编号，也不会查找“空闲磁盘”来格式化。

QEMU 在 NTFS 上生成的固定 VHD 仍可能带有 SparseFile 属性，Windows VHD
驱动会拒绝附加。辅助脚本先确认该私有文件未附加，仅对它调用 `fsutil sparse
setflag ... 0` 清除稀疏标记，再检查未压缩、未加密、非稀疏；这一步不作用于
用户输入镜像或后端的 QCOW2 层。本轮 8 个测试 VHD 均经过此处理并完成往返。

这台测试机的 8 份 chkdsk 日志均提示无法创建 VSS 快照，随后继续只读扫描、
明确报告未发现文件系统问题并返回 0。检查使用隔离的测试卷，期间没有其他
写入者；VSS 快照创建成功不属于本轮通过项。原始日志的 SHA-256 保存在结果中。

CI 已增加 Linux 完整矩阵与 Windows 原生驱动小镜像矩阵；本地验证结果不代表
远端 CI 已运行。
