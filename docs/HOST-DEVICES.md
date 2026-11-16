# 本机设备发现与临时设备验证（D0 / D1）

Windows、Linux 已提供只读的磁盘/分区枚举和共享后端保护规则。Windows GUI 增加“本机磁盘（只读）”入口。
真实本机分区的编辑、格式化、分区表修改和写回仍关闭。D1 的写入、确认、恢复仅存在于独立测试程序，且只接受工厂新建的 128 MiB VHD/loop 镜像。

## 普通使用

```bash
./xetch package
./dist/xetch-linux-x64/xetch --json device list
./dist/xetch-linux-x64/xetch --json device inspect linux:8:0
```

```powershell
.\xetch.cmd package
.\dist\xetch-windows-x64\xetch.exe --json device list
.\dist\xetch-windows-x64\xetch.exe --json device inspect windows:disk:0
```

设备 ID 仅用于本次系统枚举定位；磁盘号、设备号可能重用，不是未来的写入授权。`device.inspect` 重新枚举，返回新的 `snapshot_id`。两条命令均不需要 QEMU，不触发 UAC，也不自动挂载或卸载卷。权限或提供程序不足时返回错误/警告，不把失败当作空磁盘列表或可写状态。

GUI 的只读查看不替换当前镜像/会话，不提供写入按钮；即使镜像所需 QEMU 缺失，后端成功启动后仍可查看设备。

JSON-RPC 增加：

| 方法 | 参数 | 行为 |
| --- | --- | --- |
| `device.list` | `{}` | 返回设备、依赖关系、角色、保护原因与发现警告 |
| `device.inspect` | `{ "id": "当前枚举 ID" }` | 重新发现并查找该设备 |

参数拒绝未知字段。`device.open/write/commit/confirm/recover` 明确拒绝；`force`、伪造 `confirmed` 或实验文件系统开关均不能开放物理写入。镜像 API 仍要求普通文件，并显式拒绝 Windows 原始设备命名空间。

`system.capabilities` 保留 `physical_disks=false`、`in_place_write=false`；新增 `physical_disk_discovery`、`physical_disk_write=false` 与 `physical_disk_methods`，避免旧客户端误把发现能力解释为设备编辑能力。

## 设备信息和保护

输出为扁平 `devices` 数组，通过 `parents` 表示分区、复合设备及底层设备关系。每个设备包括当前 OS 路径、容量、分区偏移、逻辑/物理扇区、型号/序列号/稳定身份提示、分区类型/UUID、文件系统、访问路径、角色和 `protection`。稳定身份缺失或重复不能作为未来设备准入依据。

| 依据 | 当前行为 |
| --- | --- |
| Windows 系统/启动标志；Linux `/`、`/boot`、`/boot/efi`、`/efi` | 保护设备及底层依赖 |
| GPT EFI、MSR、Windows Recovery、BIOS Boot；MBR 活动/EFI/恢复类型 | 保护，无需盘符或卷标 |
| 页面文件、swap、休眠和转储配置 | 保护所在设备 |
| 程序、当前工作区和临时数据 | 保护所在设备 |
| 上述设备的底层磁盘、同盘其他分区和相关映射 | 按依赖图保守传播保护 |
| 加密/复合存储、身份重复、父设备缺失、拓扑循环或元数据不完整 | 拒绝解除保护 |
| 已挂载、未知卷状态、缺失身份、不支持或不对齐的扇区边界 | 给出具体原因；不提供写入准入 |
| 其他普通数据分区 | 同样只读，原因包含当前版本关闭设备写入 |

Linux 使用有超时/输出上限的 `lsblk`、sysfs、mountinfo、swap 和文件所在设备信息；包含 slaves 依赖与 loop 背后文件所在设备。当前仅观察本进程挂载命名空间，不能据此证明其他命名空间或所有打开句柄均为空闲。

Windows 使用内嵌的固定 PowerShell Storage/CIM 只读查询，覆盖 MSFT 磁盘/分区/卷及页面文件、转储配置；Get-Disk 未覆盖的设备保留为受限的 Win32 CIM 条目并尝试枚举其分区。查询来自系统提供程序，不能替代最终提交时的原生独占、加密/卷状态与身份复核。发现辅助进程最多运行 30 秒，输出限 8 MiB。

`complete=false` 表示仍有警告或未验证关系，不等于“磁盘为空”；`hard_protected` 表示已识别的保护/不确定关系。`write_allowed` 在本阶段对全部真实设备恒为 false。

## D1 独立验证程序

`device_validation_probe` 仅在 `validation-hooks` 下编译为 example，不进入常规 CLI 的 RPC 分发，也不进入发行包。启用该特性不会开放生产后端的设备写入方法。

工厂只在**不存在的新目录**创建普通文件、固定大小 MBR/FAT32 镜像和归属记录。Linux 只接受整盘 `/dev/loopN`，以 LOOP_GET_STATUS64 验证它确实映射到同一新建文件的设备号/inode、零偏移和完整大小；Windows 从同一归属 VHD 的 OpenVirtualDisk/GetVirtualDiskPhysicalPath 推导设备，禁止调用者传入物理磁盘路径，再验证单个 volume extent 的磁盘号、偏移和长度。文件重解析点/额外硬链接、归属或几何变化均拒绝。

Linux 用 `O_EXCL` 获取内核设备声明；Windows 先用 `FSCTL_LOCK_VOLUME` 检查并锁定归属卷，成功卸载后将整个归属测试 VHD 置为非持久离线状态，再以禁止共享的原生磁盘句柄持有独占并执行有界分区 I/O。离线状态跨 helper 退出保留，防止部分更新的 FAT 被系统重新挂载；仅测试控制器在验证安全终态后为驱动检查重新上线。独占从计划读取开始保持到提交/恢复终态或安全退出。它不构成对另一名管理员直接原始写入的防护。

Windows 首次挂载会更新 FAT FSInfo/系统元数据，因此在同一离线独占租约内先捕获新的普通 raw 基线，再从该副本生成修改计划。首次附加前为新建 MBR 盘生成非零签名，完整前置区哈希仍必须匹配。测试工厂的原始种子和基线均在操作后复核不变。

计划只比较该夹内的另一个完整普通 raw 镜像，分区表/前置保留区必须保持原哈希。I/O 限定在数据分区窗口，按 4 KiB 记录，修改总量上限 2 MiB。计划内容、目标身份、当前租约及随机计划 ID 一同生成哈希；确认必须精确匹配短语，授权随机、短期、单次且绑定当前计划。取消、新计划、过期、重启、身份变化、错误授权或重复提交均不能复用许可。

GUI 的测试构建提供真正的 WinUI 确认框：显示测试镜像、目标标识、分区范围、修改量和恢复位置，默认取消，完整短语匹配后才启用写入。生产设备浏览界面没有此写入入口。测试控制器会自动提供确认短语以验证状态机；这不是可用于真实设备的授权服务。

## 日志与故障恢复

1. 确认前只读取设备与生成内存计划；没有目标数据写请求。
2. 消耗单次授权后复核身份、持续独占权、分区边界及所有待改块旧值。
3. 在虚拟设备外的恢复目录保存完整旧值/新值、目标、事务和计划信息；校验 SHA-256，并同步记录，然后才允许第一个设备写请求。
4. 写入后同步设备，完整核对修改块，再同步提交标记。标记后的错误报告 `commit_uncertain`，保留已提交结果。
5. 返回的写入/同步错误触发经过核验的撤销；恢复再次失败时保留未完成日志、阻止新编辑。重新打开必须显式确认恢复，恢复可再次中断后重试。
6. 日志缺失、校验不符、终态冲突或目标出现非旧值/新值的其他字节时保持阻断，允许只读状态/哈希检查。测试中恢复损坏日志的控制器备份不代表产品会自动修复坏日志。

辅助进程同步完成提交或恢复后才回复；前端 stdin 关闭不会中途丢弃正在进行的提交。Windows 测试盘保持离线至安全终态后由控制器显式上线；新 helper 在恢复前不会重新挂载半更新的文件系统。强制结束 helper 会释放 OS 租约，下一进程依靠持久记录发现未完成操作。这不能替代 D2 要求的特权 helper 生命周期、全系统设备准入、启动前恢复拦截或真实掉电设计。

D1 的恢复文件位于宿主文件系统、目标是虚拟设备；并未证明它们位于不同真实物理故障域。Windows 的文件同步不等同于已证明目录项的掉电持久性。进程终止、返回 I/O 错误和模拟半块写入均不能当作真实硬件掉电验收。

## 复现

Linux 需要 Rust、Python、QEMU、util-linux 和 dosfstools；仅临时设备测试需要 root：

```bash
cargo build --locked --release --workspace --bins --features validation-hooks --example device_validation_probe
python3 scripts/devices_validate.py --inventory-only \
  --directory artifacts/device-enumeration --output artifacts/device-enumeration-results.json
sudo python3 scripts/devices_validate.py \
  --directory /tmp/xetch-new-disposable-devices --output artifacts/devices-linux-results.json
```

Windows 枚举不需要管理员。完整 VHD 测试从管理员 PowerShell 启动，只作用于新建临时 VHD：

```powershell
$env:CARGO_TARGET_DIR = "$PWD/target/device-validation-windows"
cargo build --locked --release --workspace --bins --features validation-hooks --example device_validation_probe
./scripts/windows_devices_validate.ps1 -Directory artifacts/devices-new-windows `
  -Output artifacts/devices-windows-results.json `
  -Binary target/device-validation-windows/release/xetch.exe `
  -Probe target/device-validation-windows/release/examples/device_validation_probe.exe
```

可选 `-Gui` 指定 `windows_build_gui.ps1 -Publish -SelfContained -SmokeTests` 的验证构建，以运行测试确认框和实际按钮事件。键盘、文件选择器和可访问性仍需独立交互验收。常规一键构建不需要编译/附加测试设备。

测试覆盖普通权限枚举、伪造 RPC、忙卷和第二个独占声明、取消/过期/重放、计划不可变、身份变化、前端断开、日志落盘/每批写入/目标同步/提交标记边界、EIO/ENOSPC/半块写入、恢复再次中断与损坏日志阻断；成功提交后用真实文件系统驱动读全文件、回写，并由后端再次读取。

本机验收（2026-11-16）：

| 验收项 | Linux x64 | 原生 Windows x64 |
| --- | --- | --- |
| 普通权限发现、当前系统保护、伪造 RPC 拒绝 | 6 项通过 | 6 项通过 |
| 临时设备独占、确认、提交与恢复 | 28 用例 / 104 检查组通过 | 28 后端用例通过；加 GUI 用例共 29 用例 / 125 检查组 |
| Rust 工作区全特性，含启用的集成测试 | 39 通过，0 失败/忽略 | 36 通过，0 失败/忽略 |
| Clippy 全特性/全部目标 | 通过 | 通过 |
| 搬移后的发行目录（空格/中文，清除开发环境） | 13 项通过 | 11 项通过 |
| 实际 WinUI 设备浏览 / 写入确认框 | — | 6 项 / 19 项通过 |

Windows 后端的 28 项完整矩阵先通过；修正确认短语按钮的事件时序后，单独复验 GUI 用例。两轮使用完全相同的 Rust 验证源文件和 probe SHA-256，汇总记录保留原始来源、覆盖范围和重跑说明。GUI 使用同步的 TextChanging 更新按钮，并让测试等待界面事件处理；默认取消、空/错误短语、超时、实际主按钮事件、后端单次许可和提交完整哈希均通过。

结果：[Linux loop](host-devices-linux-results.json)、[Windows VHD](host-devices-windows-results.json)、[Windows 确认框](host-devices-windows-gui-results.json)、[普通权限 Linux](host-devices-discovery-linux-results.json)、[普通权限 Windows](host-devices-discovery-windows-results.json)、[只读界面](host-devices-discovery-windows-gui-results.json)、[Linux 打包](host-devices-package-linux-results.json)、[Windows 打包](host-devices-package-windows-results.json)、[质量汇总](host-devices-quality-results.json)。总计 57 个设备用例、229 个检查组；测试结束确认无遗留挂载、loop、附加 VHD 或验证进程。

验证脚本可用 `--case NAME` 选定用例，或在结果路径旁提供 `OUTPUT.selection.json`（`{"cases":["windows-gui-consent"]}`）供限时管理员测试会话重试。选定运行明确记录 `full_matrix=false`，不能当作完整矩阵；汇总必须检查实际用例覆盖。未选择时默认执行完整矩阵。CI 已配置同一用例，远程运行结果单独认定。

原分阶段设计和后续真实数据分区门槛见 [物理分区方案](PHYSICAL-PARTITIONS-PLAN.md)。本阶段没有开放 D2。

实现依据：[Windows 卷锁定](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_lock_volume)、[卷卸载](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_dismount_volume)、[MSFT 磁盘信息](https://learn.microsoft.com/en-us/windows-hardware/drivers/storage/msft-disk)、[MSFT 分区信息](https://learn.microsoft.com/en-us/windows-hardware/drivers/storage/msft-partition)、[VHD 设备关联](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-getvirtualdiskphysicalpath)、[Linux open](https://man7.org/linux/man-pages/man2/open.2.html)、[loop 接口](https://man7.org/linux/man-pages/man4/loop.4.html)。
