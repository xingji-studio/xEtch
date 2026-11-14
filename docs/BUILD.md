# 一键构建与目录打包

入口是项目自己的 `xetch` 构建脚本，只使用 Python 标准库，不需要 Ninja。构建需要 Python 3.12+，运行打包后的程序不需要 Python、Rust 或开发工具。

Linux / macOS / WSL：

```bash
./xetch doctor          # 检查构建环境
./xetch build           # 编译当前平台，默认 Release
./xetch package         # 编译、收集运行文件并检查，输出到 dist/
```

Windows PowerShell / cmd：

```powershell
.\xetch.cmd doctor
.\xetch.cmd build
.\xetch.cmd package
```

Windows x64 默认构建后端、CLI 和 WinUI GUI；Linux/macOS 默认构建后端与 CLI，两个平台的 GUI 尚未实现。构建输出位于 `target/xetch/<平台>-<架构>/<release|debug>/`；GUI 在 `gui/`，后端在 `backend/`。

## 选择平台与组件

```bash
# WSL 调用 Windows 原生工具链，同时构建 Windows 前后端
XETCH_PLATFORM=windows ./xetch build
XETCH_PLATFORM=windows ./xetch package

# 只构建当前平台后端；也可用等价的命令行参数
XETCH_COMPONENTS=backend ./xetch build
./xetch package --components backend
./xetch build --profile debug
```

```powershell
$env:XETCH_PLATFORM = 'windows'
$env:XETCH_COMPONENTS = 'all'
.\xetch.cmd package
```

| 环境变量 | 默认值 / 用途 |
| --- | --- |
| `XETCH_PLATFORM` | 当前系统；`windows`、`linux`、`macos` |
| `XETCH_ARCH` | 当前 CPU；`x64`、`arm64` |
| `XETCH_COMPONENTS` | `auto`；可选 `all`、`backend`、`frontend`，单独 frontend 仅支持 build |
| `XETCH_PROFILE` | `release`；可选 `debug` |
| `XETCH_NTFS` | `auto`：复用匹配工作进程或尝试构建，否则明确提示 NTFS 只读；`on` 强制要求，`off` 排除 |
| `XETCH_OFFLINE` | 设为 `1` 禁用 Cargo/NuGet/QEMU/NTFS 源包的网络获取；需预先缓存依赖 |
| `XETCH_QEMU_DIR` | 指定目标平台 QEMU 目录；Windows 默认 `artifacts/windows/qemu`，POSIX 默认从 PATH 查找 |
| `XETCH_BUILD_DIR` / `XETCH_DIST_DIR` | 构建根目录 / 打包根目录，默认 `target/xetch` / `dist` |
| `XETCH_CARGO` / `XETCH_CARGO_TARGET_DIR` | 指定 Cargo 可执行文件 / 缓存目录；默认 Windows 缓存 `target/windows`，其他平台 `target` |
| `XETCH_WINDOWS_CARGO_HOME` | WSL 调用 Windows Cargo 时指定原生 Cargo 缓存，避免混用 Linux 缓存 |
| `XETCH_PYTHON` | Windows 入口指定 Python 完整路径；默认尝试 `py -3`、`python` |
| `XETCH_GUI` | 后端无参数启动时指定 GUI 可执行文件；默认查找应用目录中的前端 |

`--platform`、`--arch`、`--components`、`--profile`、`--ntfs` 与 `--offline` 可覆盖对应环境变量；`./xetch --help` 查看参数。`package --output <目录>` 指定包目录；`package --no-build` 只打包选择完全一致的已有成功构建。

目前使用目标系统及同架构的原生工具链，另支持 WSL 调用 Windows 原生构建；指定环境变量不会自动安装跨平台编译器。Windows GUI 仅支持 x64。macOS 和 arm64 分支尚未实机验证。

## 构建依赖

- 后端：Rust 1.89+；Windows 需要 MSVC C++ Build Tools 和 Windows 10/11 SDK，Linux 需要 C 链接器，macOS 需要 Xcode Command Line Tools。
- Windows GUI：.NET 8 SDK（或兼容的更新 SDK）、Visual Studio / Build Tools MSBuild、Windows SDK；首次构建恢复锁定的 NuGet 包。统一入口发布时随附 .NET、Windows App SDK 与可用的 VC 运行库。
- QEMU：编译本身不需要，打包需要 `qemu-img` 和 `qemu-storage-daemon`。Linux 可安装 `qemu-utils qemu-system-common`，macOS 可安装 QEMU。Windows 在线打包会调用 `windows_prepare_qemu.ps1` 准备固定版本，需要 7-Zip 与 SDK 的 `mt.exe`；可通过 `XETCH_7ZIP` 指定 7-Zip，或设置 `XETCH_QEMU_DIR` 使用已准备好的兼容目录。
- 可选 NTFS 写入工作进程：POSIX C 编译器、make、pkg-config；从 Linux/WSL 构建 Windows 版本还需 MinGW-w64。原生 Windows 可复用已经匹配当前源码的工作进程；首次构建可先在 WSL 执行 `python3 scripts/build_ntfs_worker.py --target windows`。工作进程二进制、配置、许可证和对应源码会一起收集，复用时校验源码及 SHA-256。详见 [NTFS 说明](NTFS.md)。

项目源码中的 `./xetch` 是构建入口，打包目录中的 `./xetch` / `xetch.exe` 是实际 CLI 和后端。

## 运行打包目录

默认目录是 `dist/xetch-windows-x64`、`dist/xetch-linux-x64` 或 `dist/xetch-macos-arm64` 等；Debug 包增加 `-debug` 后缀。整个目录可一起移动到其他位置，从任意工作目录调用其中的程序：

```text
xetch-windows-x64/
  xetch.exe                         后端与 CLI，也支持 serve --stdio
  start-gui.cmd                     Windows GUI 启动入口
  gui/                              GUI、XAML 资源和自包含运行时
  qemu/                             QEMU 与 DLL / 非系统共享库
  xetch-ntfs-worker.exe              可选 NTFS 写入组件
  ntfs-worker-{profile.json,source/,licenses/}
  check-deps.cmd / check-deps.ps1    Windows 运行依赖检查
  licenses/  LICENSE  README.txt
  runtime-files.txt  xetch-package.json
```

```powershell
.\dist\xetch-windows-x64\check-deps.cmd
.\dist\xetch-windows-x64\xetch.exe          # 不传参数时打开 GUI，也可双击
.\dist\xetch-windows-x64\xetch.exe doctor
.\dist\xetch-windows-x64\start-gui.cmd
```

```bash
./dist/xetch-linux-x64/check-deps.sh
./dist/xetch-linux-x64/xetch doctor
./dist/xetch-linux-x64/xetch serve --stdio
```

后端可执行文件 `xetch[.exe]` 不传业务参数时启动 GUI，然后退出启动进程。显式传入 `--help`、`--version`、`doctor`、`serve --stdio` 或其他 CLI 参数时仍执行对应命令；未知参数、缺少子命令和显式空参数继续报错，不打开 GUI。GUI 缺失时返回退出码 4，并提示完整打包或使用 CLI；Linux/macOS 当前没有自带前端，默认属于此情况，可通过 `XETCH_GUI` 指定兼容的前端程序。

启动路径按实际后端文件位置解析，与调用者的工作目录或程序名拼写无关：发行包查找 `gui/`，统一构建产物支持同级 `backend/` 和 `gui/`。带包清单的目录不会回退使用源码树中的前端。GUI 默认使用发起启动的后端完整路径；显式 `XETCH_BACKEND` 配置仍优先，两个覆盖路径的相对值在改变工作目录前解析。

参数列表中的程序名 `argv[0]` 不计入业务参数，遵循 [Windows 参数约定](https://learn.microsoft.com/en-us/cpp/cpp/main-function-command-line-args)。macOS 还会消费参数列表开头的历史 Finder `-psn_<数字>_<数字>`，以及 Xcode 的 `-NSDocumentRevisionsDebugMode YES|NO`，参见 [ScummVM 对这些启动参数的处理](https://github.com/scummvm/scummvm/blob/master/base/commandLine.cpp)。只处理这些明确的系统参数；不删除命令参数、文件路径或 `--` 后的内容。macOS 兼容分支有单元覆盖，尚未实机验收。

Windows 启动 GUI 时关闭句柄继承，GUI 不会占用启动者的重定向管道或控制台；CLI/RPC 输出保持原有格式。源码根目录的 `./xetch` 仍是构建工具，不是这里所说的后端可执行文件。

Windows 包包含 .NET 和 Windows App SDK 运行文件，无需预装对应运行时；GUI 要求 Windows 10 2004+ x64。Linux/macOS 包收集 QEMU 的非系统共享库，仍要求目标系统的系统 C 库及版本与构建产物兼容，建议在最低支持的系统版本构建。共享库路径仅传给子进程。显式设置的 `XETCH_QEMU_IMG`、`XETCH_QEMU_STORAGE_DAEMON`、`XETCH_NTFS_WORKER`、`XETCH_BACKEND` 优先级高于包内查找；排查时可清除这些覆盖项，Windows 可运行 `check-deps.ps1 -Isolated`。

打包会先检查暂存目录，通过后再替换已有生成包。若输出目录含用户新增文件、修改过的文件或其他内容，会拒绝替换，要求选择新目录。请将镜像和编辑会话保存在应用目录外。

## 验证

`scripts/package_validate.py` 将包复制到包含空格和中文的新目录，清除开发环境覆盖项及 PATH 工具依赖，再验证后端发现包内 QEMU/NTFS、创建 ext4、写入文件、导出 VMDK、读回内容、依赖缺失诊断及目录保护。Windows 另调用 `windows_launcher_validate.ps1` 验证直接进程、Windows Shell（双击使用的启动通道）、cmd、PowerShell 和快捷方式的无参数启动；检查正常 CLI、无效参数、JSON-RPC、缺少 GUI 的行为。传入自包含的 GUI 测试变体时，还会验证 GUI 自动寻找后端并完成 ext4 编辑。

```bash
python3 scripts/package_validate.py --package dist/xetch-linux-x64 \
  --directory artifacts/package-linux-validation --output artifacts/package-linux-results.json
```

Windows 的完整命令见 CI 配置。报告：[Linux](package-linux-results.json)、[Windows](package-windows-results.json)、[Windows 启动矩阵](launcher-windows-results.json)。macOS、独立干净系统和 GUI 文件选择器交互仍需后续验收。
