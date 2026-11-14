# 实现结构与后续工作

`crates/xetch-core`：应用入口、错误、QEMU 适配器、NBD 块设备、分区窗口与解析、文件系统适配器、工作区。

FAT 继续使用 `fatfs`；`extended.rs` 通过固定版本 `fstool = 0.5.0` 的原生 Rust 实现读取 exFAT/ext4/NTFS。只启用 std/exfat/ext/ntfs 特性，不引入 FUSE、内核挂载、Linux 工具依赖或 libguestfs。Linux 独立格式化工具仅用于测试。

`vendor/fatfs` 保留 MIT 0.3.6 源码，局部修复特殊目录项不能带 LFN 前缀、FAT32 根级目录的 `..` 必须指向零簇的问题；FAT12/16/32 独立一致性和 GUI 输出回归覆盖该路径。

`vendor/fstool` 保留 MIT 原包及来源校验记录，Cargo 路径补丁补齐 exFAT 目录项改名、连续目录访问与 Unicode up-case 表校验；文件数据不会因改名删除重建。格式化复用其完整映射表的扇区接口，通过适配窗口支持 none/MBR/GPT。ext4 写入预检路径 inode 的保护标志和共享扩展属性块释放条件，读写均检查内部日志无待恢复事务；元数据保留、索引目录及回退由独立测试验证，见 [exFAT/ext4 说明](EXFAT-EXT4.md)。

新建 ext4 使用同一库的原生格式化器，限制为固定的 4 KiB/256 字节 inode 配置，先规划并处理不足以容纳元数据的末尾块组。只有新建的全零临时文件才启用 prezeroed 优化；最终通过通用发布路径导出 IMG/VMDK。分区类型随文件系统选择，卷 UUID、GPT GUID 与 exFAT 序列号来自系统随机源。`ext_acl.rs` 负责默认 ACL 的磁盘编码校验和权限过滤，适配器处理 setgid 组继承；库补丁修复空名称 ACL 属性解析并校验外置属性块 CRC32C。

exFAT/ext4 写入由应用层显式实验参数控制，成功操作记录 `experimental_filesystem`。NTFS 写入通过 `ntfs.rs` 调用独立 NTFS-3G 2026.9.28 工作进程，其自定义设备回调由父进程通过管道访问分区窗口。库默认宿主设备 I/O 被禁用。覆盖普通文件保留原 inode/MFT 和其安全描述符及其他属性，失败时丢弃子覆盖层。组件评估、支持边界与验证见 [NTFS 说明](NTFS.md)。

`crates/xetch-cli`：clap 参数入口、JSON-RPC stdio 传输。两者均调用 `xetch_core::execute(method, params)`。

`apps/windows`：C# / WinUI 3 原生 GUI。`BackendClient` 负责独立后端进程与串行 JSON-RPC 请求，磁盘和文件系统逻辑不进入界面代码。异步调用避免阻塞 UI，按钮忙碌状态防止重复请求；关闭窗口时先等待当前操作完成。标准输出只承载协议，诊断流持续排空以免管道阻塞。保存选择器只返回路径，后端执行拒绝覆盖发布。普通构建不含 GUI 集成测试入口。

## 数据与提交

源 IMG/VMDK → base.qcow2 → edit-1.qcow2 → edit-2.qcow2。源镜像通过格式适配器始终只读打开；写入只开放给本次新建的覆盖层。

`workspace.json` 保存源镜像依赖与指纹、当前 head 和成功操作清单。`.lock` 使用跨平台文件锁，序列化同一会话的访问。

一次文件操作：检查文件系统与写入模式 → 创建以旧 head 为 backing 的子覆盖层 → 修改该层 → 卸载 FAT 或刷新适配器元数据 → NBD FLUSH → 回收 QEMU → 文件刷盘及 QCOW2 check → 再次验证源指纹 → 保存新 manifest 并替换旧 manifest。失败时不推进 head；异常终止可能留下未引用的覆盖层，恢复时忽略它们。暂不自动清理，以免误删恢复材料。

导出写入目标目录内的临时文件，检查格式与虚拟容量，刷盘并复核源镜像后，以拒绝覆盖方式发布最终文件。Unix 额外同步父目录；Windows 的目录刷盘与真实掉电一致性尚未验证，不能承诺事务式断电恢复。

原始 NBD 客户端仅实现 fixed-newstyle、simple replies、READ/WRITE/FLUSH；单次传输上限 1 MiB。Windows 与 Linux 已有运行证据，macOS 待验。本地回环 TCP 是当前传输方案，随机导出名不是完整的本机多用户安全边界，发布前需评估本地 socket/命名管道或认证隔离。

Windows QEMU 命令行 JSON 使用 ASCII Unicode 转义，诊断按有损 UTF-8 解码保留错误原因。固定发行包还需项目内 UTF-8 进程清单配置，解决其 ANSI 文件接口接收 UTF-8 JSON 文件名的问题。准备脚本保留原安装包校验值、逐个工具修改前后哈希，不更改机器级代码页。该配置是测试/开发依赖，不作为已签名发布包。

子进程启动等待 QEMU 写出就绪 pidfile 后才连接，避免监听启动前的连接被代理重置。当前逐次块 I/O 尚未做页缓存优化，大文件导入性能仍需改进。

## 后续优先级

1. 扩大 NTFS 真实 Windows 来源样本、复杂元数据及 chkdsk 验证，推进 Windows GUI 选择器和完整输入交互验收。
2. 对真实 VMDK 与更多 FAT12/16 样本测试，增加 4Kn、分卷边界、异常描述文件和输入资源限额。
3. 增加后台作业、进度、可取消的导出及父子进程共同退出机制。
4. 扩大 exFAT/ext4 真实来源样本、共享外置扩展属性和故障注入覆盖；完成 macOS 实机验收、QEMU 打包和签名。
5. 完善工作区路径迁移、覆盖链压缩、撤销与清理；优化完整源哈希对大镜像的影响。
6. 完善 Windows GUI，接入 Linux/macOS 原生前端。现有分区表修改、分区缩放、系统挂载和原地写回另行验收。
