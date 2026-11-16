# 提交边界、随机碎片压力与 ext4 日志配置

本轮验收在先前的大文件、系统互操作与 QEMU I/O 故障矩阵上，增加提交边界、
可复现的随机操作和实际碎片卷，以及 CSUM_V2 / 2 KiB 的内核兼容验证。
结果分别记录在 [Linux](advanced-fs-linux-results.json) 和
[Windows 原生后端](advanced-fs-windows-results.json)报告中。

## 本轮结果

| 验证 | Linux x64 | Windows 原生 x64 |
| --- | --- | --- |
| 提交边界与保护检查 | 110 组通过 | 110 组通过 |
| 随机碎片压力 | 2 种文件系统 × 2 种子 × 1000 次，通过 | 同左，通过 |
| 快照内核读写往返 / 最终重开核验 | 252 / 4 次，通过 | 252 / 4 次，通过 |
| ext4 日志配置及回放 | 5 种配置、71 组通过 | 同左，通过 |
| 实际 QSD 端口冲突及重试上限 | 3 个场景通过 | 3 个场景通过 |
| Rust 单元与集成测试，含显式运行的 ignored 测试 | 30 通过，0 跳过 | 28 通过，0 跳过 |
| WinUI 控制器、渲染及后端集成 | 不适用 | exFAT / ext4 各 15 项通过 |

完整矩阵每平台 11 个案例、229 组检查；两个平台合计 8000 次混合操作。
实际碎片文件分别有 exFAT 114 个物理区间、ext4 Linux 45 / Windows 42 个物理
区间，整个压力轮次保持布局及数据不变。两平台每个种子均完成 327–333 次成功
文件写入、168–170 次删除和 251–257 次文件/目录改名或移动，另含预期冲突。

汇总见 [质量报告](advanced-fs-quality-results.json)，端口竞态原始结果见
[Linux](advanced-nbd-startup-linux-results.json)和
[Windows](advanced-nbd-startup-windows-results.json)。fmt、Clippy、Python 编译、
CI YAML 与差异空白检查均通过，未遗留本轮测试的挂载点、后端、QEMU 或 GUI
进程。各轮分别核对普通构建与故障构建的 SHA-256；GUI 使用同一 Windows
普通后端，故障变体保持独立。

本轮最终结果来自四组隔离运行，合并时校验程序哈希与逐项操作日志计数；
此前被中断、触发端口竞态或使用旧随机分布的运行未计入通过结果。
先前超过 4 GiB 文件与 Windows 自带 exFAT 驱动报告保留为旧二进制的历史证据，
没有标记为本轮程序的重复验收。

## 提交边界

`validation-hooks` 是显式选择的 Cargo feature，普通构建不读取这些故障环境
变量。验证构建与普通构建使用不同 target/runtime 目录；测试还向普通构建传入
故障变量，确认操作成功且没有触发故障记录。报告保存两份程序的 SHA-256。

每种文件系统覆盖编辑、新建格式化、镜像导出和文件导出。故障点包括：

- 待提交记录持久化、覆盖层刷盘及保留、manifest 临时文件刷盘。
- manifest 原子替换后，以及目录同步调用返回后。
- 输出暂存记录建立、raw 文件分配及格式化、QEMU 转换启动及完成。
- 文件导出复制完成、最终路径发布前后，以及目录同步调用返回后。

每个点分别返回 I/O 错误或暂停，由控制器只终止该次后端进程。检查提交点前
保持旧结果，提交点后保留完整新结果；输入镜像、旧覆盖层和无关文件保持不变。
重开后检查数据、fsck、继续编辑、暂存材料回收和拒绝覆盖已有目标。
另测同一输出/工作区的并发拒绝、发布前出现的用户文件，以及暂存目录里的
未知文件保护。每个平台每种文件系统共 55 组边界及安全检查。

本轮修复和补齐：

1. 将 manifest 临时文件加入待提交记录，进程终止后只回收记录所指的文件。
2. 新建/导出采用可识别、带锁的输出暂存目录，重试时回收同一输出的未发布材料。
   锁文件与意向 JSON 分离，避免 Windows 字节范围锁阻止读取记录；获取锁前后
   比较 nonce，拒绝使用已经被删除并重建的目录。
3. 原子替换/发布已完成后的刷盘或清理错误返回 `commit_uncertain`，提示先检查
   工作区或输出。此时不会删除已经发布的结果，也不会将其描述为已回滚。
4. `qemu-img` 与 QSD 均使用受管理的子进程，避免后端结束后转换继续运行。
5. 并行压力运行触发 NBD 临时端口被抢占的启动竞态。仅对 QSD 在握手前明确
   报告的监听端口占用最多尝试 5 次，不重试文件系统读写或其他启动错误。
   专用代理实际占住端口并启动真实 QEMU，验证一次冲突后恢复、持续冲突在
   5 次后报错，以及无关启动错误只启动一次。

暂存目录只接受已知的普通文件；未知内容会被保留并返回冲突。不会按文件名
通配扫描并删除其他用户文件。新建镜像的 raw 中间文件位于输出父目录，因此
目标文件系统也需要支持该临时文件的逻辑大小。

## 随机与碎片化压力

夹具由 Linux 内核创建：交错写入并逐文件 fsync，填到接近满盘，再删除交替
文件并写入 8 MiB 文件。通过独立 exFAT FAT 链解析或 debugfs extent 映射，
确认该文件有多个不连续物理区间，避免用逻辑上的删除/重建代替实际碎片。

每种文件系统执行两个固定种子 `0x5eed`、`0xc0ffee`，每个种子 1000 次混合
操作。操作包括新增、大小变化的覆盖、删除、目录创建/删除、文件和带内容目录
跨父目录移动、大小写改名、Unicode 名称和预期冲突；逐项保存操作日志。
工作负载版本 2 只在目录上限处限制新建目录，继续随机执行写入、覆盖和删除；
每个 1000 次轮次还检查至少 100 次写入、50 次删除和 50 次改名请求，防止
目录上限使后续压力退化为单一操作。报告同时记录这些请求的预期拒绝次数。

每 16 次操作执行：

1. 导出并运行独立 fsck，用独立解析器/debugfs 对照参考模型，核对完整目录树和
   每个文件的全部内容，并确认固定大文件的物理碎片布局没有被改变。
2. Linux 内核挂载该副本，检查所有随机文件、所有保留的锚点文件及碎片文件的
   全量 SHA-256、完整目录集合，写入反馈文件后卸载。
3. 转换为独立 VMDK，重新建立工作区并继续随机操作，在 128 层上限前压缩链。
4. 检查原始输入 SHA-256 不变、无待提交记录或 manifest 临时文件残留。

结束后还独立核对最后一次压缩、重开的结果。默认清理旧轮次的测试快照和工作区，
保留输入夹具、操作日志和最终镜像；`--keep-artifacts` 保留所有中间材料。
运行次数、种子、实际请求方法计数、拒绝次数和快照往返次数均写入报告。

Windows GUI 另使用实际碎片 exFAT 卷和 2 KiB / CSUM_V2 ext4 卷，分别运行
控制器、实际 XAML 渲染、文件导入导出、恢复和拒绝操作后的回退检查。
结果见 [exFAT](advanced-windows-gui-exfat-results.json)和
[ext4](advanced-windows-gui-ext4-results.json)；导出镜像再由 Linux 独立 fsck、
解析器/debugfs 核对内容。此项不包含文件选择器或鼠标键盘输入自动化。

## CSUM_V2 与 2 KiB 块

分别测试 CSUM_V2 的 1/2/4 KiB 块、2 KiB 下的 32 位和 64 位日志标签，以及
2 KiB 的 CSUM_V3。每个配置由系统格式化并经真实内核写入，后端覆盖和移动目录，
再由内核全量读回、写入反馈，后端继续编辑，期间运行独立 e2fsck。

CSUM_V2 夹具从系统生成的干净、空日志转换：确认 `s_start=0`，切换日志特性
并重算超级块 CRC。后端必须按 v2 写出日志。验证副本回滚目录原位置块，并将
后端日志标为待恢复，必须由实际内核回放恢复目录。另翻转 v2 标签校验和、同时
保持描述符 CRC 正确，确认内核拒绝该损坏事务。

当前 Linux 会在加载/回放旧 v2 日志后将新日志写入升级为 v3；报告明确记录
升级前后的版本。该测试验证真实内核读取、回放 v2 及后续双向读写，尚未使用
旧内核生成并持续写入 v2 的实机样本。调用顺序参见 Linux
[ext4 日志加载](https://github.com/torvalds/linux/blob/v6.12/fs/ext4/super.c)与
[JBD2 特性处理](https://github.com/torvalds/linux/blob/v6.12/fs/jbd2/journal.c)。

## 复现

```bash
cargo build --locked --release --workspace
cargo build --locked --release -p xetch-cli --features validation-hooks --target-dir target/validation-linux
python3 scripts/advanced_filesystems_validate.py --suite all \
  --directory artifacts/advanced-linux --output artifacts/advanced-linux-results.json

# WSL：选择能看到测试目录的发行版，以 root 运行受限的挂载辅助脚本。
python3 scripts/advanced_filesystems_validate.py --suite all --wsl-distribution Ubuntu-22.04 \
  --directory /tmp/xetch-advanced-linux --output artifacts/advanced-linux-results.json

# Windows 后端和验证变体须先用原生 MSVC Cargo 构建。
python3 scripts/advanced_filesystems_validate.py --suite all --wsl-distribution Ubuntu-22.04 \
  --binary target/windows/release/xetch.exe \
  --validation-binary target/validation-windows/release/xetch.exe \
  --directory artifacts/advanced-windows --output artifacts/advanced-windows-results.json
```

Windows 变体编译时使用 `--features validation-hooks --target-dir target/validation-windows`。
压力测试可用 `--operations`、`--seeds`、`--checkpoint-every` 调整；可选择
`--suite boundaries|stress|journals` 单独运行。测试依赖 QEMU、exfatprogs、e2fsprogs
与 Python 3.10+；Windows 后端的独立检查在 Linux/WSL 宿主执行，使用 Windows
原生后端与 QEMU。Linux 本轮使用可由 WSL 辅助进程访问的本地 `/tmp`，避免
DrvFS 的覆盖链元数据开销。挂载权限只授予受限辅助脚本。

端口竞态回归还需构建验证专用代理，普通发布包不包含它：

```bash
cargo build --locked --release -p xetch-core --example qsd_fault_proxy
python3 scripts/nbd_startup_validate.py --directory artifacts/nbd-startup-linux \
  --output artifacts/nbd-startup-linux-results.json
```

## 边界

故障点从归属记录持久化后开始；未模拟任意指令处的终止或真实掉电、缓存丢失、
乱序和撕裂写入。Windows 的目录同步目前仍为空操作，相关故障点表示同步调用
返回，不代表已经证明目录项的断电持久性。启动时 Job Object 归属建立前的竞态、
初始化归属记录期间的中断、macOS/ARM64 实机及更大容量仍需后续验收。
随机测试是有明确种子和次数的有限压力运行，未作为 24 小时持续运行或全部
文件系统特性兼容性的保证。CI 已加入 Linux 完整矩阵和 Windows 原生提交边界
回归测试；尚未触发远端 CI。

初始化或清理归属记录内部的任意指令处终止也未逐点覆盖。意向记录缺失、损坏
或目录内容不可识别时，程序保留材料并拒绝自动回收；需要检查该目录后处理。
