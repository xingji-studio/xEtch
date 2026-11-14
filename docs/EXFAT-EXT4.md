# exFAT / ext4 实现与验证

验证日期：2026-11-14。两种文件系统均由跨平台 Rust 后端直接访问分区窗口，无需挂载、管理员权限或 Linux 文件系统工具。修改仍需显式开启实验选项，并且只写入本次 QCOW2 子覆盖层。

## 本次补齐

| 操作 | exFAT | ext4 |
| --- | --- | --- |
| 浏览、导出、新增、覆盖、创建和删除空目录 | 支持 | 支持 |
| 文件改名与跨目录移动 | 支持，包括仅改变大小写 | 支持，名称区分大小写 |
| 带内容目录改名与跨父目录移动 | 支持，按簇身份拒绝移入自身后代 | 支持，更新目录关系与链接计数 |
| 覆盖已有目标 | `put --replace`；改名目标必须空闲 | `put --replace`；改名目标必须空闲 |
| 新建文件系统 | 无分区表、MBR、GPT 的 IMG/VMDK，实验能力 | 无分区表、MBR、GPT 的 IMG/VMDK，实验能力 |

exFAT 改名直接移动目录项集合，不删除重建文件；保留数据起始簇、属性、时间、有效数据长度、分配标志及非名称次级项。文件名冲突按卷自己的 up-case 表判断，避免使用 Unicode 全量大写把 `ß` 与 `SS` 错误合并。修复了标准 5836 字节表末尾 U+FFFF 的解码问题，缺失、截断或校验和错误的表会报错，不再退回 ASCII 比较。新建卷使用完整的标准表，并写入正确的分区偏移。

连续分配的文件和目录按 NoFatChain 与数据长度读取，不依赖未使用的 FAT 项。向连续目录追加条目前，先为原有簇建立 FAT 链并更新父目录项校验和，保留目录内容。独立测试覆盖零 FAT 项的连续目录、有效数据长度后的零填充，以及长名称导致的目录扩展。[exFAT 规范](https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification)定义了这些字段和大小写映射规则。

ext4 覆盖保持同一 inode；已有硬链接同步看到新内容。独立样本验证了 mode、32 位 UID/GID、链接计数、POSIX ACL、inode 内及外置扩展属性的保留；文件修改时间、变更时间与父目录时间相应更新。新建文件/目录继承父目录默认 ACL，权限按文件 0644、目录 0755 过滤，并继承 setgid 目录的组和子目录 setgid 位；新建 UID 默认为 0。CLI 尚未提供 ACL 或扩展属性编辑命令。

修复适配器将零长度属性名误当终止项的缺陷：ext4 的 ACL 属性使用命名空间编号加空后缀，必须按四字节全零标记判断结束。ACL 继承按内核定义的磁盘 version 1 编码处理；与用户态 version 2 编码不同，参见 [ext4 ACL 数据定义](https://github.com/torvalds/linux/blob/master/fs/ext4/acl.h)和[初始化实现](https://github.com/torvalds/linux/blob/master/fs/ext4/acl.c)。外置属性块读取增加 CRC32C 检查，无效默认 ACL 或损坏属性块在修改提交前拒绝。测试通过 debugfs 的 `-r` 明确读写磁盘编码，并检查其命令诊断，避免只依赖退出码。

## 新建 ext4 镜像

格式化仍为实验能力，使用原生 Rust 实现，不依赖宿主系统的 mkfs。固定配置为 4 KiB 块、256 字节 inode、约每 16 KiB 一个 inode、5% 保留块、extents、metadata_csum、sparse_super、内部 JBD2 日志和 `/lost+found`。卷 UUID 为随机 v4，日志 UUID 与文件系统一致；日志取文件系统块数的 1/32，限制为 1024..32768 块。字段和日志规则见 [内核超级块文档](https://www.kernel.org/doc/html/latest/filesystems/ext4/super.html)与 [JBD2 文档](https://www.kernel.org/doc/html/latest/filesystems/ext4/journal.html)。

格式化只作用于新建、全零的临时文件。文件系统按 8 块对齐，若最后一个块组不足以容纳元数据，会去掉该部分块组；镜像和分区仍保持请求容量，末尾可能留有少量未使用空间。格式化块数必须小于 2^32，实际大容量验收覆盖到 5 GiB，未把这个结构上限作为完整兼容性承诺。现有分区就地格式化未开放。

另外修复规划器在接近 2^32 块时计算末尾块组范围的整数溢出，并加入仅规划边界的单元测试；这不代表已对该容量创建真实镜像。

新建 MBR 正确使用 ext4 `0x83`、exFAT `0x07`、FAT32 `0x0c`；GPT 的 ext4 使用 Linux filesystem GUID，exFAT/FAT32 使用 Microsoft basic data GUID。GPT 磁盘和分区标识采用随机 v4 GUID；exFAT 新建卷也改为随机序列号，避免默认固定标识。

## 使用

```bash
xetch workspace create source.vmdk --workspace edit-session
xetch --experimental-filesystems file mkdir --workspace edit-session --path /archive
xetch --experimental-filesystems file rename --workspace edit-session --source /documents --destination /archive/documents
xetch --experimental-filesystems file put --workspace edit-session --source local.txt --destination /archive/documents/data.txt --replace
xetch apply --workspace edit-session --output edited.vmdk
xetch --experimental-filesystems create --output exfat.img --size 128MiB --table gpt --filesystem exfat
xetch --experimental-filesystems create --output linux.vmdk --size 256MiB --table gpt --filesystem ext4
```

每次修改都必须带 `--experimental-filesystems`；JSON-RPC 对应 `experimental_filesystems: true`。Windows GUI 勾选实验选项后可编辑这两种卷，也可在新建镜像对话框选择 exFAT 或 ext4。后端 `system.capabilities` 的 `filesystem_operations` 列出各自操作；实验状态仍由 `filesystem_write_experimental` 表示。

## 拒绝路径与限制

exFAT 写入拒绝脏卷、介质错误及双 FAT 的 TexFAT。ext4 读取和写入都检查文件系统状态、RECOVER 位和内部 JBD2 日志；即使文件系统超级块标记干净，日志仍有待回放事务或错误时也拒绝访问，不自动修复或回放。拒绝外部日志及尚未支持的日志特性（包括 fast commit）。JBD2 的字节序和日志字段依据 [Linux 内核文档](https://www.kernel.org/doc/html/latest/filesystems/ext4/journal.html)；检查并非只依赖 `s_start == 0`。

ext4 写入还检查适配器支持的卷特性，并保护路径上的 immutable、append-only、加密、verity 和 casefold inode。删除共享外置扩展属性块的最后一个 inode 链接暂时拒绝，避免候选库的无条件释放路径影响其它 inode。符号链接不作为普通文件覆盖；文件名限制为 255 个 UTF-8 字节。任一阶段失败均丢弃本次子覆盖层，manifest 不推进。

这仍是实验实现：未承诺所有 ext4/exFAT 特性、真实 Windows/macOS 格式化样本、超过 4 GiB 的单文件、恶意镜像隔离或真实断电恢复。macOS 实机验收尚未完成。Windows GUI 已验控制器和真实 XAML 渲染，文件选择器、对话框与鼠标键盘输入仍需人工验收。

## 验证证据

[Linux 4 KiB 块结果](extended-linux-results.json)与 [Windows 原生结果](extended-windows-results.json)各包含 exFAT 12 组、ext4 12 组检查，并检查 exFAT 三种分区表布局的格式化与编辑结果。Windows 操作由原生 `xetch.exe` 和 QEMU `.exe` 完成，Linux 独立工具只负责造样和复核输出。

[Linux 1 KiB 块结果](extended-linux-1k-results.json)重复通过同一矩阵，覆盖另一种常用 ext4 布局。

检查覆盖改名/移动、连续分配、大小写冲突、带内容目录和后代路径拒绝、元数据保留、索引目录、空间耗尽后部分写入回退、日志待恢复拒绝、IMG/VMDK 读回及源 SHA-256 不变。导出的卷分别经过 `fsck.exfat -n`、`e2fsck -fn`；文件内容和元数据由独立 exFAT 解析器及 `debugfs` 读取。

新建矩阵的 [Linux 结果](format-linux-results.json)与 [Windows 原生结果](format-windows-results.json)各覆盖 16 个场景：两种文件系统、none/MBR/GPT、64 MiB 最小容量、512 字节尾部、微小末尾块组、多块组、直接新建 VMDK 和 5 GiB VMDK。每个场景检查新建、编辑、删除后的原始镜像；多块组 ext4 额外用备用超级块运行 e2fsck，独立核对分区类型、两份 GPT 校验和、文件系统标识、日志、完整 up-case 表及 exFAT 两份引导区校验和。创建还测试未开启实验选项、非法容量、输出冲突和依赖故障无残留。

Windows GUI 新建 [ext4](windows-gui-create-ext4-results.json)和 [exFAT](windows-gui-create-exfat-results.json)各通过 18 项控制器及实际窗口检查，使用与新建对话框相同的选项控件和提交方法；新建源镜像与编辑另存的输出均通过[独立检查](windows-exports-check-results.json)。没有把控制器测试视为文件选择器或鼠标键盘输入验收。

基础 FAT32 GUI 回归额外发现 fatfs 0.3.6 给 `.`/`..` 添加了非法 LFN 前缀，并为 FAT32 根目录下的 `..` 写入根簇号。已以小范围 MIT 源码补丁修复；[Linux](fat-linux-results.json)与 [Windows](fat-windows-results.json)的 FAT12/16/32 输出均通过 fsck.fat 和 mtools，GUI 默认 FAT32 的新建与另存也完成独立检查。FAT 非 BMP 名称的现有限制仍会明确拒绝并保留检查点。

Windows GUI 的 [exFAT](windows-gui-exfat-results.json)与 [ext4](windows-gui-ext4-results.json)各通过 14 项检查，包括初始只读、实验开关启用写入、导入/改名、文件及镜像导出、会话恢复、错误展示、检查点回退和窗口渲染。GUI 导出的镜像也通过[独立一致性与内容检查](windows-exports-check-results.json)。

复现时使用新目录：

```bash
python3 scripts/extended_validate.py --directory artifacts/extended-linux --output artifacts/extended-linux-results.json
python3 scripts/extended_validate.py --directory artifacts/extended-linux-1k --ext-block-size 1024 --output artifacts/extended-linux-1k-results.json
# 在 WSL 中调用 Windows 原生后端，独立检查工具仍在 Linux 运行：
python3 scripts/extended_validate.py --binary target/windows/release/xetch.exe --directory artifacts/extended-windows --output artifacts/extended-windows-results.json
python3 scripts/format_validate.py --directory artifacts/format-linux --large-volume --output artifacts/format-linux-results.json
python3 scripts/format_validate.py --binary target/windows/release/xetch.exe --directory artifacts/format-windows --large-volume --output artifacts/format-windows-results.json
```

需要 QEMU、Python 3.12+、`exfatprogs` 和 `e2fsprogs`，不需要 root。可用 `--mkfs-exfat`、`--fsck-exfat` 指定工具路径。大容量矩阵需要为镜像输出预留空间；独立检查在临时目录提取一个卷，检查后自动清理。原生 Windows CI 用 `format_validate.py --produce-only` 创建及编辑，将结果上传后在 Linux 用 `--check-directory` 独立检查。CI 已覆盖新建矩阵和 GUI 新建流程，尚未触发远端运行。

## 依赖补丁

固定 `fstool 0.5.0`，MIT 源码及许可证保存在 `vendor/fstool`，Cargo 使用路径补丁；来源和原包 SHA-256 见 [来源记录](../vendor/fstool-origin.json)，修改范围见 [补丁说明](../vendor/README.md)。上游 0.5.1 起提高最低 Rust 版本到 1.95；本次保留项目的 Rust 1.89 要求，仅修改所需适配器与错误类型，没有整体升级。NTFS 仍沿用独立 NTFS-3G 工作进程。
