# xEtch

跨平台离线 IMG / VMDK 编辑器，包含 Rust 后端、CLI 和首版 Windows WinUI 3 原生 GUI。Windows 原生及 Linux 的基础 P0 已通过。

当前可完成：打开 IMG/VMDK → 建立编辑会话 → 修改 FAT/NTFS 或实验性 exFAT/ext4 文件 → 保存为新的 IMG/VMDK。源镜像不会被写回；NTFS 写入需安装可选的 NTFS-3G 工作进程。

## 构建与依赖

统一入口需要 Python 3.12+ 和 Rust 1.89+。Windows x64 默认同时构建后端、CLI 和 GUI；Linux/macOS 默认构建后端与 CLI。

```bash
./xetch doctor
./xetch build
./xetch package
# WSL 构建 Windows 版本：XETCH_PLATFORM=windows ./xetch package
```

Windows 使用 `.\xetch.cmd build`、`.\xetch.cmd package`。默认包位于 `dist/xetch-<平台>-<架构>/`，收集后端、QEMU、可选 NTFS 工作进程及 Windows GUI 的自包含运行时。整个文件夹可以移动后运行，不需要 Python 或构建工具；Windows 用 `start-gui.cmd` 启动界面。运行 `check-deps.cmd`（Windows）或 `./check-deps.sh`（Linux/macOS）检查缺失依赖。

包内 `xetch.exe` 无参数运行或双击时会打开 GUI；带 `doctor`、`--help`、`serve --stdio` 等参数时仍作为 CLI/后端运行。缺少前端时会明确报错，可用 `XETCH_GUI` 指定前端路径。源码根目录的 `./xetch` 是构建入口，继续使用 `build/package/doctor` 命令。

通过 `XETCH_PLATFORM`、`XETCH_COMPONENTS=all|backend|frontend`、`XETCH_PROFILE=release|debug` 选择构建；环境要求、目录结构和所有选项见 [构建与打包说明](docs/BUILD.md)。Linux/macOS 包仍依赖兼容的系统 C 库。实际操作需要 QEMU 的 `qemu-img`、`qemu-storage-daemon`，无需挂载镜像或管理员权限。

NTFS 写入工作进程在 Linux/WSL 或 macOS 构建，需要 Python 3.12+、C 编译器、make 和 pkg-config。Windows 交叉构建还需 MinGW-w64：

```bash
python3 scripts/build_ntfs_worker.py
python3 scripts/build_ntfs_worker.py --target windows
```

原生产物默认放入 `target/release`；Windows 产物放入 `target/windows/release`。将 `xetch-ntfs-worker[.exe]` 放在后端旁，或设置 `XETCH_NTFS_WORKER`。固定的 NTFS-3G 2026.9.28 源包校验、构建参数、许可证与对应源码材料一同生成。没有版本匹配的工作进程时 NTFS 保持只读。组件选型和验证见 [NTFS 说明](docs/NTFS.md)。

直接 `cargo build --release` 仍可用，产物是 `target/release/xetch[.exe]`。可通过 `XETCH_QEMU_IMG` 和 `XETCH_QEMU_STORAGE_DAEMON` 指定工具完整路径；否则优先查找后端旁的 `qemu/`，再查找 PATH。

已实际验证 Linux / WSL2 + QEMU 8.2.2，以及 Windows 11 x64 + 项目内 QEMU 11.1.0 UTF-8 兼容配置，两个系统的目录打包与移动后运行也已验证。macOS 实机运行、干净系统验收、签名安装包和真实断电恢复仍待完成。CI 配置包含原生 Windows 集成验收和跨平台文件系统互操作检查；本次没有触发远端 CI。

### Windows 依赖

部分 QEMU Windows 发行包的 ANSI 文件接口不能直接处理 JSON 中的中文路径。`scripts/windows_prepare_qemu.ps1` 校验固定版本的安装包，使用 7-Zip 提取工具，并通过 Windows SDK 的 `mt.exe` 为**项目内工作副本**加入 UTF-8 进程清单。它不会执行安装程序、修改系统区域设置或修改已安装的 QEMU；修改后的二进制哈希写入依赖配置记录。需要 Windows 10 1903+ 的 UTF-8 清单支持，目前实际验收系统为 Windows 11。

```powershell
./scripts/windows_prepare_qemu.ps1 -SevenZip 'C:\Program Files\7-Zip\7z.exe'
$env:XETCH_QEMU_IMG = "$PWD/artifacts/windows/qemu/qemu-img.exe"
$env:XETCH_QEMU_STORAGE_DAEMON = "$PWD/artifacts/windows/qemu/qemu-storage-daemon.exe"
cargo build --release
./target/release/xetch.exe doctor
```

本次环境的原生构建产物位于 `target/windows/release/xetch.exe`，可直接配合上述两个环境变量运行。其它支持完整 Unicode 路径的 QEMU 发行包也可以通过环境变量指定，但需运行验证脚本确认兼容性。

### Windows GUI

推荐通过统一入口构建和打包，构建需要 .NET SDK、Visual Studio / Build Tools 的 MSBuild 和 Windows SDK；包内附带 .NET 与 Windows App SDK 运行文件：

```powershell
.\xetch.cmd package
.\dist\xetch-windows-x64\start-gui.cmd
```

界面支持打开与新建镜像、分区和文件浏览、导入导出、目录创建、改名删除、会话恢复和另存。开发环境自动查找项目内后端及 QEMU；可用 `XETCH_BACKEND` 指定后端。请保留 GUI 输出的完整目录。详见 [GUI 构建与验证说明](apps/windows/README.md)。

勾选实验选项后，可编辑 exFAT/ext4，并在新建镜像对话框选择 exFAT 或 ext4。

## 完整使用流程

以下命令在同一目录执行，`config.txt` 为本地准备导入的文件。输出路径和会话目录必须尚不存在。请将程序加入 PATH，或将 `xetch` 替换为构建产物的路径。

```bash
xetch create --output disk.img --size 64MiB --table gpt
xetch inspect disk.img --json
xetch workspace create disk.img --workspace edit-session
xetch file mkdir --workspace edit-session --partition 1 --path /settings
xetch file put --workspace edit-session --partition 1 --source config.txt --destination /settings/config.txt
xetch file list --workspace edit-session --partition 1 --path /settings
xetch file get --workspace edit-session --partition 1 --source /settings/config.txt --destination extracted.txt
xetch plan --workspace edit-session
xetch apply --workspace edit-session --output disk-edited.vmdk
xetch file list disk-edited.vmdk --partition 1 --path /settings
```

`--replace` 允许覆盖**会话中的文件**。宿主机输出文件始终拒绝覆盖。`file rename`、`file remove`、`convert`、`sector read` 的选项可用 `--help` 查看。

FAT 文件夹目前支持同级改名；跨目录移动文件夹会明确拒绝。文件可以跨目录移动。文件夹删除要求为空。FAT 名称支持中文等 BMP 字符，非 BMP 字符（例如部分 emoji）暂时拒绝；exFAT/ext4 不受这一库限制。

```bash
xetch file rename --workspace edit-session --source /settings/config.txt --destination /settings/config-old.txt
xetch convert disk.img --output converted.vmdk
xetch sector read disk.img --offset 0 --length 512
xetch serve --stdio
```

`--table none` 创建独立文件系统卷，索引为 0；MBR/GPT 的分区索引从 1 开始。仅有一个卷时可省略 `--partition`。容量的 K/M/G、KiB/MiB/GiB 均表示二进制单位；最小创建容量 64 MiB。

## exFAT、ext4 与 NTFS

exFAT/ext4 可直接浏览和导出。写入需要每个命令显式传入 `--experimental-filesystems`，修改仍写入独立覆盖层，成功的实验操作在 `plan` 中标注文件系统。会话可以照常另存。

```bash
xetch workspace create linux.vmdk --workspace linux-edit
xetch file list --workspace linux-edit --path /
xetch --experimental-filesystems file put --workspace linux-edit --source config.txt --destination /config.txt
xetch apply --workspace linux-edit --output linux-edited.vmdk
xetch --experimental-filesystems file rename --workspace linux-edit --source /documents --destination /archive
xetch --experimental-filesystems create --output exfat.img --size 64MiB --table gpt --filesystem exfat
xetch --experimental-filesystems create --output linux-new.vmdk --size 256MiB --table gpt --filesystem ext4
```

exFAT、ext4 均支持新建独立卷、MBR 或 GPT 的 IMG/VMDK。exFAT 文件与带内容目录均可改名和移动，名称冲突按卷的 Unicode 映射判断。ext4 支持区分大小写的文件/目录移动，保留已有权限、硬链接、ACL 和扩展属性，并支持新建文件/目录的默认 ACL、setgid 组继承。NTFS 格式化尚未开放。脏 exFAT 拒绝写入；带错误或需日志回放的 ext4 拒绝读取和写入，不自动回放日志。详细支持边界及 Linux/Windows 验证见 [exFAT/ext4 说明](docs/EXFAT-EXT4.md)。

NTFS 使用独立的 NTFS-3G 工作进程进行普通文件读写，无需实验开关。支持文件创建/替换、文件夹创建、文件与空目录删除、文件改名及移动；覆盖保留原 MFT、安全描述符、ADS 和 EA。只接受正常关闭的离线卷；拒绝脏卷、休眠卷、系统元数据路径、目录改名及压缩/EFS/reparse 文件修改。Linux 和 Windows 原生读写验证、故障回退及独立工具检查见 [NTFS 报告](docs/NTFS.md)。

## 当前能力边界

| 项目 | 状态 |
| --- | --- |
| raw IMG、常见 VMDK 读取 | 已实现，格式由内容识别 |
| 五种 VMDK 子格式的覆盖层写入与另存 | Linux 与 Windows 原生样本验证通过；另含 5 GiB 分卷偏移检查 |
| FAT12/16/32 文件浏览、导入导出、目录创建、删除、重命名 | 已实现；端到端验收重点覆盖 FAT32 |
| 新建 FAT32 + MBR/GPT/无分区表 | 已实现 |
| MBR 主分区、GPT 解析和 GPT 校验和验证 | 已实现，逻辑扇区固定 512 字节 |
| exFAT、ext4 | 原生浏览/导出；实验性新增、覆盖、目录创建、删除、文件和目录改名/移动及新建三种布局；Windows 与 Linux 互操作检查通过 |
| NTFS | 原生浏览/导出；可选 NTFS-3G 2026.9.28 普通文件写入，Windows/Linux 样本验证通过 |
| 导出 | raw 或单文件 monolithicSparse VMDK |
| 原地写回、现有分区调整、逻辑分区、快照链、加密盘、物理盘 | 尚未支持 |
| Windows GUI | WinUI 3 首版已构建及集成验证；选择器与鼠标键盘交互待人工验收 |
| Linux/macOS GUI、系统挂载、后台任务进度与取消 | 后续阶段 |

修改会话是持久化 QCOW2 覆盖链，不会完整复制源镜像。每个成功操作保留一个检查点，目前上限 128 次；达到上限后导出并建立新会话。导出可能扫描大量数据并需要额外空间。

源镜像及其 VMDK extent 文件会记录完整 SHA-256、长度和修改时间，每次打开会话及提交前复核。大型镜像的完整校验会较慢，这是当前正确性优先的实现。会话引用绝对路径，编辑期间不要移动源镜像或会话目录；所有关联文件都需保留。

此版本面向可信、离线、正常关闭的镜像。请先停止使用该镜像的虚拟机；QEMU 文件锁和指纹检查无法阻止所有不遵守锁协议的外部写入。它尚未经过恶意镜像解析隔离、真实断电和大规模兼容性验收。

## 验证

```bash
cargo fmt --all -- --check --config-path rustfmt.toml
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p xetch-core --test backend -- --include-ignored --test-threads=1
python3 scripts/p0_validate.py --output artifacts/p0.json
python3 scripts/cli_validate.py --binary target/release/xetch --output artifacts/cli.json
python3 scripts/fat_validate.py --directory artifacts/fat-linux --output artifacts/fat-linux-results.json
python3 scripts/filesystems_validate.py --fixtures artifacts/fs-fixtures --results-dir artifacts/fs-linux --output artifacts/filesystems-linux.json
python3 scripts/extended_validate.py --directory artifacts/extended-linux --output artifacts/extended-linux-results.json
python3 scripts/extended_validate.py --directory artifacts/extended-linux-1k --ext-block-size 1024 --output artifacts/extended-linux-1k-results.json
python3 scripts/format_validate.py --directory artifacts/format-linux --large-volume --output artifacts/format-linux-results.json
python3 scripts/ntfs_validate.py --directory artifacts/ntfs-linux --output artifacts/ntfs-linux-results.json --large-volume
```

Windows 原生验证：

```powershell
./scripts/windows_validate.ps1 -Binary target/release/xetch.exe -QemuDirectory artifacts/windows/qemu -Output artifacts/windows/p0-results.json
./scripts/windows_filesystems_validate.ps1 -Binary target/release/xetch.exe -QemuDirectory artifacts/windows/qemu -Fixtures artifacts/fs-fixtures -ResultsDirectory artifacts/windows/filesystems -Output artifacts/windows/filesystems-results.json
```

Python 验证脚本在 Linux 运行，还需要 `dosfstools`、`mtools`、`e2fsprogs`、`ntfs-3g`、`exfatprogs`。`filesystems_validate.py` 用独立工具生成 Windows 验证所需的 fixtures；目录必须为新路径。Windows 保存的镜像可通过 `scripts/check_windows_exports.py` 在 Linux 独立检查。普通单元测试不需要 QEMU；标记 ignored 的集成测试必须显式运行，不能将跳过视为通过。

详细结论见 [P0 报告](docs/P0.md)，接口见 [协议说明](docs/PROTOCOL.md)，实现与未完成事项见 [架构说明](docs/ARCHITECTURE.md)。原有 Apache-2.0 许可证保持不变；QEMU 与 GPL NTFS 工作进程作为独立程序，分发时需保留各自声明和对应源码材料。
