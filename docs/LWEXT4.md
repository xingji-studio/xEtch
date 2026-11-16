# BSD lwext4：xattr、ACL、大文件与打包

`xetch-ext4-worker` 是可选的独立进程，使用固定版本 lwext4 的 BSD 模块和独立补写的 BSD extent、xattr、POSIX ACL 实现。上游 `ext4_extent.c`、`ext4_xattr.c` 两个 GPL 实现仍不进入 vendor 或编译输入。主项目继续采用原有 Apache-2.0 许可证，所选第三方 BSD 声明随包保留。

原有 fstool 保持默认，CLI、JSON-RPC 和 GUI 继续使用相同请求；普通读取无需实验开关，修改 ext4 仍需显式实验许可。工作进程通过有界管道访问父进程的分区窗口，普通镜像操作仍只写 QCOW2 子层并导出新文件。

## 构建、打包与启用

```bash
./xetch build
XETCH_EXT4=on ./xetch package
# Linux/WSL 构建 Windows 后端及 WinUI 前端：
XETCH_PLATFORM=windows XETCH_EXT4=on ./xetch package

XETCH_EXT4_ENGINE=lwext4 ./dist/xetch-linux-x64/xetch file list disk.img
```

Windows 可用 `.\xetch.cmd package --ext4 on`，再设置 `$env:XETCH_EXT4_ENGINE="lwext4"` 启动包内 CLI 或 GUI。`XETCH_EXT4=auto|on|off` 是构建策略，与运行时引擎选择分开：auto 复用或编译可选组件，on 要求组件可用，off 排除组件。未选择 lwext4 时使用原有 fstool。

POSIX 使用 GNU 兼容 C 编译器；Windows 可复用源文件/头文件/profile/许可证全部匹配的二进制，使用 MinGW，或通过已配置的 WSL + MinGW 构建。独立命令仍为 `python3 scripts/build_ext4_worker.py [--target windows]`。`XETCH_EXT4_CC` 指定需要编译时的 C 编译器。

包内包含 `xetch-ext4-worker[.exe]`、`ext4-worker-profile.json` 和 `ext4-worker-licenses/`。来源清单、源码/头文件 SHA-256、许可证 SHA-256 和本机运行测试约束复用；打包前重新检查。运行无需 Python、编译器或测试工具。Windows 的 GUI 自包含运行时继续随包提供，运行依赖检查会报告缺失或不兼容的 worker。

worker 握手要求 `bsd-xattr-acl-v1`、协议 1 和 xattr/ACL 能力位，旧 worker 在任何块 I/O 前被拒绝。`XETCH_EXT4_WORKER` 可覆盖默认的后端旁查找路径。`--with-probe` 仅构建开发验证程序，打包排除 `examples/`。

## 属性与权限语义

- 独立 BSD xattr API 实现 inode 内属性与单个外置属性块的读取、列表、设置、替换、删除、空值和二进制值。外置块按规范排序、生成哈希和校验和，共享块修改采用复制写入，释放按引用计数处理。
- 兼容旧有带符号字符名称哈希与现代无符号字符哈希；新写入采用无符号形式。支持 UTF-8 属性名，保留已有属性字节与命名空间。
- 替换普通文件和改名保留 inode、硬链接、已有所有者、权限、ACL 与扩展属性。
- 新文件/目录解析父目录默认 POSIX ACL，按创建模式过滤 access ACL；子目录继续继承默认 ACL。setgid 目录的组继承使用完整 32 位 GID，新对象的 UID 继续采用项目既有的 0。
- 格式化仍使用原有实现。普通文件编辑继续通过既有公共命令执行；通用 xattr API 的额外验证通过不分发的独立测试程序完成。

格式依据为内核公布的 [ext4 属性布局](https://www.kernel.org/doc/html/latest/filesystems/ext4/attributes.html)。ACL 使用 ext4 版本 1 磁盘表示；内核用户态 xattr 接口的版本 2 由独立内核测试进行转换验证。

本轮还修复了大文件的 LARGE_FILE 超级块标记、UID/GID 高位、旧 inode 短校验和，以及文件写入错误被 inode 释放结果覆盖的问题。extent 分配、零初始化和顺序数据 I/O 以至多 256 个文件系统块为批次，不改变父进程的刷新顺序和提交规则。

## 验证与复现

```bash
python3 scripts/build_ext4_worker.py --with-probe
python3 scripts/build_fs_validation_tools.py
cargo build --locked --release --workspace --bins --example qsd_fault_proxy --example ext4_protocol_probe
cargo build --locked --release -p xetch-cli --features validation-hooks --target-dir target/validation-linux
python3 scripts/lwext4_validate.py --suite all --directory /tmp/xetch-lwext4 \
  --output artifacts/lwext4.json --wsl-distribution Ubuntu-22.04 \
  --e2fsck artifacts/tools/e2fsprogs/sbin/e2fsck
```

Windows 原生后端验收增加 `--binary target/windows/release/xetch.exe`，并将 `--directory` 设为 Windows 可访问的项目目录；构建其对应 worker/probe 和 validation-hooks 后端。内核挂载仅作用于已标记目录下的新建普通镜像，实际编辑由原生 Windows 后端/worker 完成。

独立 oracle 固定为 e2fsprogs 1.47.4，校验下载 SHA-256 后构建；它仅用于测试，不链接进 BSD worker，也不进入应用包。旧 e2fsck 1.46.5 会误报现代内核生成的中文属性名哈希。样本明确选择已准入的文件系统特性，不把新版 mke2fs 的 metadata_csum_seed/orphan_file 默认值当作已支持功能。

元数据矩阵覆盖 1/2/4 KiB 块、128/256 字节 inode、关闭 metadata_csum、extra_isize 为 0 的旧 inode、现代及旧名称哈希、共享块 COW/释放、默认 ACL/setgid、损坏属性 CRC 和无效 ACL 的失败回退。另有跨 2/4 GiB 的稀疏边界回归，完整大文件验收会全量写入、导出、哈希、截短、重新覆盖、改名，并执行真实内核往返及再次全量哈希。

本机验收结果（2026-11-16）：

| 项目 | Linux x64 | 原生 Windows x64 |
| --- | --- | --- |
| lwext4 综合验收 | 23 用例 / 330 检查组，通过 | 23 用例 / 330 检查组，通过 |
| 完整大文件 | 4,297,064,585 字节，全量读写与内核往返通过 | 同尺寸，全量读写与内核往返通过 |
| 随机混合操作 | lwext4 2,000 次 + fstool 对照 2,000 次 | lwext4 2,000 次 + fstool 对照 2,000 次 |
| Rust 工作区测试（含显式启用的集成测试） | 32 通过，0 失败/忽略 | 30 通过，0 失败/忽略 |
| Clippy 全特性/全部目标 | 通过 | 通过 |
| 移动目录后的发行包 | 12 项通过 | 10 项通过 |
| Windows GUI | — | 20 项通过 |

综合验收包括元数据与 ACL、extent、日志转义、协议/资源边界、提交与 I/O 故障、碎片和随机压力。两平台连同原实现对照共执行 8,000 次混合操作、504 次压力快照内核往返及 8 次最终压力内核检查。大文件两平台的完整 SHA-256 均为 `de0b77b0fa8a6692120e8cfefac14f713495e24ea00559ecf2441d83238df2a9`。完整 Rust 测试后又针对最终 C worker 各重跑 2 项集成测试；这些重复项未计入工作区测试数量。

结果记录：[Linux](lwext4-advanced-linux-results.json)、[Windows](lwext4-advanced-windows-results.json)、[Windows GUI](lwext4-advanced-windows-gui-results.json)、[Linux 打包](lwext4-package-linux-results.json)、[Windows 打包](lwext4-package-windows-results.json)、[质量与来源汇总](lwext4-advanced-quality-results.json)。结果中保留实际测试二进制及源文件哈希。Windows 后端重新构建产生的 PE/调试元数据和同一 Cargo 目录的路径大小写差异单独记录；比较仅归一化这些已核实字段，其余字节须相同。

一键构建和打包已实际运行，产物位于 `dist/xetch-linux-x64/` 与 `dist/xetch-windows-x64/`。搬到包含空格/中文的目录并移除开发环境变量后，分别验证后端操作和 Windows GUI；缺少 worker/QEMU 时依赖检查能报出具体问题。测试辅助程序和独立 e2fsck 不进入发行包。此处是本机结果，更新后的远程 CI 尚未运行。

## 支持边界

传统 xattr 布局最多 256 项、每 inode 一个外置块，容量仍受块大小限制；EA_INODE、RichACL 继承、加密、verity 和复杂安全策略执行不在此实现范围内。写入要求 Linux inode 布局及既有卷状态/特性检查，脏卷和需恢复日志的输入仍拒绝访问。

extent 的深度与资源上限沿用原 P0，实际样本覆盖不等于穷尽所有边界。macOS/ARM64 和真实物理断电尚未验收；有限随机测试、进程终止和 I/O 注入不能替代这些验收。Windows 目录同步的现有边界继续保留。

本机物理分区编辑是 [另一项分阶段规划](PHYSICAL-PARTITIONS-PLAN.md)，当前版本不开放设备写入。历史初版结果保留在 [P0 报告](LWEXT4-P0.md)。
