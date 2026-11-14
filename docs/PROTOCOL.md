# 本地协议 v1

入口：`xetch serve --stdio`。UTF-8、每行一个 JSON-RPC 2.0 对象，响应同样每行一个对象；stdout 只承载协议。单条请求最多 1 MiB。超限返回错误并结束会话。暂不支持批量请求。

```json
{"jsonrpc":"2.0","id":1,"method":"image.inspect","params":{"image":"/absolute/disk.vmdk"}}
```

成功返回 `{"jsonrpc":"2.0","id":1,"result":{...}}`。错误包含数值 `error.code`、`error.message`，以及 `error.data.code` 业务码。无 id 的合法通知执行后不返回响应。

所有路径都在后端所在机器解释。推荐绝对路径。磁盘内路径使用 `/`，拒绝 `..`、反斜杠、控制字符和非法 FAT 字符。大小与偏移是无符号 64 位 JSON 整数，客户端应保留整数精度；不使用浮点数参与磁盘计算。

| 方法 | params |
| --- | --- |
| system.capabilities | `{}` |
| image.inspect | `image` |
| image.create | `output, size`，可选 `table`（默认 gpt）、`format`、`filesystem`（默认 fat32）、`experimental_filesystems` |
| image.convert | `image, output`，可选 `format` |
| workspace.create | `image, workspace` |
| workspace.plan | `workspace` |
| workspace.apply | `workspace, output`，可选 `format` |
| file.list | `image` 或 `workspace` 二选一，可选 `partition, path`（默认 /） |
| file.get | 同上，另需 `path, destination`（宿主机新文件） |
| file.put | `workspace, path, source`（宿主机文件），可选 `partition, replace, experimental_filesystems` |
| file.mkdir / file.remove | `workspace, path`，可选 `partition, experimental_filesystems` |
| file.rename | `workspace, path, destination`（镜像内路径），可选 `partition, experimental_filesystems` |
| sector.read | `image, offset, length`（1..65536），响应中 `hex` 为十六进制内容 |

`image.create` 默认创建 FAT32，支持 none/mbr/gpt；`filesystem: "exfat"` 或 `"ext4"` 也支持这三种布局，要求 `experimental_filesystems: true`。size 至少 64 MiB 且按 512 字节对齐。`format` 为 raw/vmdk；未指定时按输出扩展名推断。ext4 新建固定采用 4 KiB 块、256 字节 inode、内部日志和元数据校验和；尚未提供格式化参数调优。创建只发布新镜像，不提供已有分区就地格式化。`file.put` 默认拒绝覆盖，replace 只影响镜像内目标。

原生读取支持 FAT12/16/32、exFAT、ext4、NTFS。exFAT/ext4 每次写入都需要请求参数 `experimental_filesystems: true`；CLI 对应 `--experimental-filesystems`。NTFS 写入依赖可选的版本匹配 NTFS-3G 工作进程，无需实验开关；缺失时返回 unsupported。实验写入成功后，`manifest.changes` 的对应项包含 `experimental_filesystem`（值为 exFAT/ext4），旧会话仍可读取。

`system.capabilities` 的 `filesystem_read_write` 动态包含 FAT 和可用时的 NTFS，另有 `filesystem_read`、`filesystem_write_experimental`、`filesystem_read_only`。`ntfs_write` 和 `ntfs_engine` 表示 NTFS-3G 是否可用。前端应据此展示模式。运行 `serve` 时 CLI 的全局实验开关不会替客户端设置请求参数。

附加字段 `filesystem_operations` 按文件系统列出操作名：`list/get/put/mkdir/remove`、`rename-file/rename-directory/move-file/move-directory`；exFAT/ext4 另有 `create-none/create-mbr/create-gpt`。它描述操作范围，不取消实验开关或卷状态检查。

当前请求串行执行，每次写入自动提交一个工作区检查点。`workspace.apply` 导出新镜像，不清除会话，不写回源镜像。取消、异步作业、进度事件及大文件专用传输尚未实现；当前文件内容通过本地文件路径传递。

FAT 的 `file.rename` 支持文件移动和同级文件夹改名；跨父目录移动文件夹返回 unsupported。exFAT/ext4 支持文件与带内容目录的改名和跨父目录移动，沿用实验开关，拒绝移入自身后代及覆盖已有目标。exFAT 按卷 up-case 表判断名称，允许仅改变大小写；ext4 区分大小写。NTFS 支持文件改名/移动，目录改名返回 unsupported。NTFS 拒绝系统元数据路径、脏卷、休眠卷和压缩/EFS/reparse 文件修改，详见 [NTFS 支持边界](NTFS.md)。exFAT/ext4 的连续分配、日志及元数据限制见 [支持说明](EXFAT-EXT4.md)。

JSON-RPC 错误：-32700 解析失败，-32600 请求结构错误，-32601 方法不存在，-32602 参数错误，-32000 操作失败。业务码包括 invalid_argument、unsupported、conflict、dependency_failure、io_error、invalid_json。

CLI 退出码：0 成功；2 参数错误（含 clap 参数解析）；3 不支持；4 I/O 或依赖错误；5 会话或内容冲突。应用错误以 JSON 写入 stderr；clap 的帮助、参数错误采用其标准文本格式。`--json` 输出紧凑 JSON，默认输出缩进 JSON。
