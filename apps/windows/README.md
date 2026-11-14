# xEtch Windows GUI

WinUI 3 / C# / .NET 8 的 Windows 原生前端。无管理员清单，不挂载镜像；所有实际镜像操作交给 `xetch serve --stdio`，与 CLI 共用后端。

[实际界面预览](../../docs/windows-gui-preview.png)。

已实现：打开 IMG/VMDK、选择分区、路径导航、文件导入/导出、新建文件夹、重命名、删除、查看更改、恢复编辑会话、另存新镜像，以及新建 FAT32 或实验性 exFAT/ext4 镜像（none/MBR/GPT）。NTFS 写入依赖可选的版本匹配 NTFS-3G 工作进程；exFAT/ext4 写入在界面显式开启实验选项，支持文件和目录改名。操作期间禁用冲突按钮，窗口等待操作完成后才关闭。

## 构建和启动

需要 Windows 10 2004+（已验 Windows 11 x64）、.NET SDK、Visual Studio / Build Tools 的 MSBuild 与 Windows SDK。构建锁定 Windows App SDK 1.8.260101001 和 SDK BuildTools 10.0.26100.7463。项目不注册 MSIX，需保留完整目录，不能单独移动 EXE。

推荐在项目根目录执行 `.\xetch.cmd build` 或 `.\xetch.cmd package`。打包后运行 `dist/xetch-windows-x64/start-gui.cmd`，先用 `check-deps.cmd` 检查依赖；包内包含 .NET、Windows App SDK、XAML 资源、后端与 QEMU，运行无需预装 .NET SDK 或对应运行时。参见 [统一构建说明](../../docs/BUILD.md)。

也可双击或无参数运行包内 `xetch.exe`，由后端找到并启动 GUI；从 cmd、PowerShell、Windows Shell 或快捷方式启动都不依赖工作目录。正常 CLI 和 `serve --stdio` 调用不会打开 GUI。`XETCH_GUI` 可指定另一个前端可执行文件。

分别构建时，先构建 Rust 后端，按根 README 准备 QEMU。若需要 NTFS 编辑，在 Linux/WSL 运行 `python3 scripts/build_ntfs_worker.py --target windows`，将生成的 `xetch-ntfs-worker.exe` 放在 `xetch.exe` 旁。以下开发脚本默认使用已安装的 .NET 8 运行时，统一入口会发布自包含版本。

```powershell
cargo build --locked --release
./scripts/windows_build_gui.ps1
./scripts/windows_run_gui.ps1
```

GUI 按以下顺序查找后端：`XETCH_BACKEND` → GUI 旁的 `backend/xetch.exe` → 同目录 `xetch.exe` → 带包清单的上级目录 `xetch.exe` → 统一构建目录中的同级 `backend/xetch.exe` → 开发目录下 `target/windows/release/xetch.exe` 或 `target/release/xetch.exe`。QEMU 可通过环境变量指定；开发模式自动查找项目内 `artifacts/windows/qemu`，部署目录则查找后端旁的 `qemu` 子目录。移动后的发行包不回退查找源码目录。

会话默认保存至 `%LOCALAPPDATA%/xEtch/Workspaces`，可用 `XETCH_WORKSPACE_ROOT` 指定目录。界面“查看更改与会话位置”提供恢复位置。支持 `--image <完整路径>` 或 `--workspace <完整路径>` 启动。会话更改在另存后仍保留，可以继续编辑；源镜像和全部关联文件需要保留原路径。

文件选择器采用 Windows App SDK 1.8 的 `Microsoft.Windows.Storage.Pickers`，只取得路径，由后端创建新文件。目标已存在时明确拒绝，不会为了满足无覆盖规则删除文件选择器产生的占位文件。[API 文档](https://learn.microsoft.com/en-us/windows/apps/develop/files/pickers-save-file)。

## 验证

```powershell
./scripts/windows_build_gui.ps1 -SmokeTests -OutputDirectory target/gui/windows-smoke
./scripts/windows_gui_validate.ps1 -Binary target/gui/windows-smoke/xEtch.Windows.exe -Backend target/release/xetch.exe -Image artifacts/fs-fixtures/ntfs.img -Directory artifacts/windows/gui-validation -Output artifacts/windows/gui-results.json
./scripts/windows_gui_validate.ps1 -Binary target/gui/windows-smoke/xEtch.Windows.exe -Backend target/release/xetch.exe -Image artifacts/fs-fixtures/exfat.img -FileSystem exFAT -Directory artifacts/windows/gui-exfat -Output artifacts/windows/gui-exfat-results.json
./scripts/windows_gui_validate.ps1 -Binary target/gui/windows-smoke/xEtch.Windows.exe -Backend target/release/xetch.exe -Image artifacts/fs-fixtures/ext4.img -FileSystem ext4 -Directory artifacts/windows/gui-ext4 -Output artifacts/windows/gui-ext4-results.json
./scripts/windows_gui_validate.ps1 -Binary target/gui/windows-smoke/xEtch.Windows.exe -Backend target/release/xetch.exe -FileSystem ext4 -CreateImage -Directory artifacts/windows/gui-create-ext4 -Output artifacts/windows/gui-create-ext4-results.json
./scripts/windows_gui_validate.ps1 -Binary target/gui/windows-smoke/xEtch.Windows.exe -Backend target/release/xetch.exe -FileSystem exFAT -CreateImage -Directory artifacts/windows/gui-create-exfat -Output artifacts/windows/gui-create-exfat-results.json
```

测试变体在真实 WinUI 窗口中运行同一控制器，验证 NTFS 分区展示、文件导入与列表刷新、重命名、文件导出、镜像另存、会话恢复、错误提示、失败回退和按钮忙碌状态，并将实际 XAML 视觉树渲染为 PNG。正常构建不包含测试入口。测试会话与输出均限制在显式指定的新测试目录。

新增 [exFAT 结果](../../docs/windows-gui-exfat-results.json)与 [ext4 结果](../../docs/windows-gui-ext4-results.json)，各 14 项通过，包含初始只读和实验开关启用写入；导出的 IMG 另经 Linux 独立一致性及内容检查。

新建 [ext4](../../docs/windows-gui-create-ext4-results.json)与 [exFAT](../../docs/windows-gui-create-exfat-results.json)各 18 项检查通过：实验选项未开启时只提供 FAT32；开启后新建表单增加 exFAT/ext4；使用与对话框相同的控件和提交方法完成新建、编辑及另存。`-CreateImage` 不能与 `-Image` 同时使用，`-Table none|mbr|gpt` 可切换布局。新建源镜像和另存镜像均通过独立一致性检查。测试没有模拟文件选择器、对话框按钮或鼠标键盘输入。

[默认 FAT32 新建流程](../../docs/windows-gui-create-fat32-results.json)另有 16 项检查；修复底层特殊目录项后，源镜像和编辑另存的输出也通过 fsck.fat 与 mtools 独立检查。

[本机运行记录](../../docs/windows-gui-results.json)。电脑控制工具因 WSL 路径初始化错误无法使用，已完成原生窗口和控制器集成检查；**文件选择器、对话框及鼠标键盘交互仍需人工验收**。当前还没有任务百分比、取消、撤销、拖放、多选、安装程序和签名。Linux/macOS 原生前端尚未开始。
