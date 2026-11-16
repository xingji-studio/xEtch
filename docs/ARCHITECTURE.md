# 实现结构与后续工作

`crates/xetch-core`：应用入口、错误、QEMU 适配器、NBD 块设备、分区窗口与解析、文件系统适配器、工作区。

FAT 继续使用 `fatfs`；`extended.rs` 通过固定版本 `fstool = 0.5.0` 的原生 Rust 实现读取 exFAT/ext4/NTFS。只启用 std/exfat/ext/ntfs 特性，不引入 FUSE、内核挂载、Linux 工具依赖或 libguestfs。Linux 独立格式化工具仅用于测试。

`vendor/fatfs` 保留 MIT 0.3.6 源码，局部修复特殊目录项不能带 LFN 前缀、FAT32 根级目录的 `..` 必须指向零簇的问题；FAT12/16/32 独立一致性和 GUI 输出回归覆盖该路径。

`vendor/fstool` 保留 MIT 原包及来源校验记录，Cargo 路径补丁补齐 exFAT 目录项改名、连续目录访问与 Unicode up-case 表校验；文件数据不会因改名删除重建。格式化复用其完整映射表的扇区接口，通过适配窗口支持 none/MBR/GPT。ext4 写入预检路径 inode 的保护标志和共享扩展属性块释放条件，读写均检查内部日志无待恢复事务；元数据保留、索引目录及回退由独立测试验证，见 [exFAT/ext4 说明](EXFAT-EXT4.md)。

新建 ext4 使用同一库的原生格式化器，限制为固定的 4 KiB/256 字节 inode 配置，先规划并处理不足以容纳元数据的末尾块组。只有新建的全零临时文件才启用 prezeroed 优化；最终通过通用发布路径导出 IMG/VMDK。分区类型随文件系统选择，卷 UUID、GPT GUID 与 exFAT 序列号来自系统随机源。`ext_acl.rs` 负责默认 ACL 的磁盘编码校验和权限过滤，适配器处理 setgid 组继承；库补丁修复空名称 ACL 属性解析并校验外置属性块 CRC32C。

exFAT/ext4 写入由应用层显式实验参数控制，成功操作记录 `experimental_filesystem`。NTFS 写入通过 `ntfs.rs` 调用独立 NTFS-3G 2026.9.28 工作进程，其自定义设备回调由父进程通过管道访问分区窗口。库默认宿主设备 I/O 被禁用。覆盖普通文件保留原 inode/MFT 和其安全描述符及其他属性，失败时丢弃子覆盖层。组件评估、支持边界与验证见 [NTFS 说明](NTFS.md)。

`ext4_worker.rs` 提供可选的 lwext4 引擎，默认保留 fstool 作为原实现和对照。worker 只接收操作参数与卷容量，通过有界 stdio 块请求访问父进程的分区窗口，不能直接打开镜像。父进程复用卷状态、inode/xattr 检查、子进程生命周期和提交策略，并在写入应答前下传 NBD FLUSH。固定版本的 BSD lwext4 源文件和独立 BSD extent、xattr 和 ACL 模块编译为 `xetch-ext4-worker`，上游 GPL extent/xattr 源文件不进入源码包或编译输入；清单哈希检查、源码补丁与范围见 [lwext4 说明](LWEXT4.md)。CLI、GUI 和 JSON-RPC 请求无需变化。

`crates/xetch-cli`：clap 参数入口、JSON-RPC stdio 传输。两者均调用 `xetch_core::execute(method, params)`。

`apps/windows`：C# / WinUI 3 原生 GUI。`BackendClient` 负责独立后端进程与串行 JSON-RPC 请求，磁盘和文件系统逻辑不进入界面代码。异步调用避免阻塞 UI，按钮忙碌状态防止重复请求；关闭窗口时先等待当前操作完成。标准输出只承载协议，诊断流持续排空以免管道阻塞。保存选择器只返回路径，后端执行拒绝覆盖发布。普通构建不含 GUI 集成测试入口。

## 数据与提交

源 IMG/VMDK → base.qcow2 → edit-1.qcow2 → edit-2.qcow2。源镜像通过格式适配器始终只读打开；写入只开放给本次新建的覆盖层。

`workspace.json` 保存源镜像依赖与指纹、当前 head 和成功操作清单。`.lock` 使用跨平台文件锁，序列化同一会话的访问。

一次文件操作：检查文件系统与写入模式 → 持久化 `.pending-edit.json` 记录本次私有层及 manifest 临时文件 → 创建以旧 head 为 backing 的子覆盖层 → 修改该层 → 卸载 FAT 或刷新适配器元数据 → NBD FLUSH → 回收 QEMU → 文件刷盘及 QCOW2 check → 再次验证源指纹 → 保留新层并同步目录 → 原子替换 manifest → 删除待提交记录。提交点之前失败保持旧 head；提交已发布后的同步/清理错误返回 `commit_uncertain`，恢复以磁盘实际 manifest 为准。

异常终止后，重开读取实际 manifest 并检查其 backing chain，仅删除待提交记录明确标识、且未被提交链引用的私有层。提交已完成但记录尚未删除时保留该层；不会扫描并删除其它 `edit-*.qcow2` 文件。Linux 对目录执行 fsync；Windows 真实掉电一致性仍需验收。

新建和导出使用目标父目录中的专用暂存目录，保存格式化 raw 中间文件或转换结果，检查格式与容量、刷盘并复核源镜像后，以拒绝覆盖方式发布。意向记录包含目标名与 nonce；独立锁文件避免 Windows 对锁定记录的跨句柄读取限制。获取锁前后校验 nonce，恢复只清理已知内部文件，未知内容保留并报冲突。成功/普通失败清理暂存目录；中断后，完整且可识别的意向记录允许同一输出的下一次尝试回收材料，记录缺失或损坏时保留并拒绝处理。已发布目标始终保留。Unix 额外同步父目录；Windows 的目录刷盘与真实掉电一致性尚未验证，不能承诺事务式断电恢复。

原始 NBD 客户端仅实现 fixed-newstyle、simple replies、READ/WRITE/FLUSH；单次传输上限 1 MiB。Windows 与 Linux 已有运行证据，macOS 待验。本地回环 TCP 是当前传输方案，随机导出名不是完整的本机多用户安全边界，发布前需评估本地 socket/命名管道或认证隔离。

Windows QEMU 命令行 JSON 使用 ASCII Unicode 转义，诊断按有损 UTF-8 解码保留错误原因。固定发行包还需项目内 UTF-8 进程清单配置，解决其 ANSI 文件接口接收 UTF-8 JSON 文件名的问题。准备脚本保留原安装包校验值、逐个工具修改前后哈希，不更改机器级代码页。该配置是测试/开发依赖，不作为已签名发布包。

子进程启动等待 QEMU 写出就绪 pidfile 后才连接，避免监听启动前的连接被代理重置。临时端口在释放预留监听器后仍可能被其他进程占用；只对握手前 QSD 明确报告的监听端口占用最多启动 5 次，不重试数据读写或其他启动失败。QSD 和 qemu-img 的 RAII 包装负责正常清理；Linux 用 PR_SET_PDEATHSIG 和父 PID 复核，Windows 用非继承、KILL_ON_JOB_CLOSE 的 Job Object，在后端异常退出后终止已归属的子进程。实际故障矩阵分别杀死后端或 QSD，检查无存活的 QSD/代理；转换启动也加入提交边界中断矩阵。

NBD 为文件系统的扇区/块读取保留最多 8 个 1 MiB LRU 页，并合并最多 1 MiB 连续写入。读、非连续定位及 FLUSH 前先提交待写数据；写入使重叠读缓存失效，显式 FLUSH 继续下传 QEMU，保留文件系统日志的刷新顺序。常驻缓存和缓冲合计至多 9 MiB，替换读页时短暂额外分配至多 1 MiB，数据不按整个镜像缓存。超过 4 GiB 的完整读写和故障测试覆盖这些路径。

ext4 检查完整的 JBD2 超级块 CRC，按既有特性生成日志标签及事务 CRC，并更新超级块 CRC；内核写入、后端编辑、内核再次读写以及内核强制回放后端日志均已纳入验收。普通读取/编辑仍拒绝待回放的外来日志。

## 后续优先级

1. 扩大 NTFS 真实 Windows 来源样本、复杂元数据及 chkdsk 验证，推进 Windows GUI 选择器和完整输入交互验收。
2. 对真实 VMDK 与更多 FAT12/16 样本测试，增加 4Kn、分卷边界、异常描述文件和输入资源限额。
3. 增加后台作业、进度与可取消的导出；扩大子进程启动边界和 macOS 异常退出验证。
4. 扩大 exFAT/ext4 真实来源样本、共享外置扩展属性和故障注入覆盖；完成 macOS 实机验收、QEMU 打包和签名。
5. 完善工作区路径迁移、覆盖链压缩、撤销与清理；优化完整源哈希对大镜像的影响。
6. 完善 Windows GUI，接入 Linux/macOS 原生前端。现有分区表修改、分区缩放、系统挂载和原地写回另行验收。

物理分区编辑的保护、授权与恢复设计见 [规划文档](PHYSICAL-PARTITIONS-PLAN.md)，当前实现不开放设备写入。
