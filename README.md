<img src="app-icon.png" width="96" alt="Windle" />

# Windle

系统清理与维护工具，同时支持 **macOS 与 Windows**（界面与功能一致，平台差异见下）。基于 **React + Tauri v2** 构建，帮助你安全、透明地回收磁盘空间、卸载应用、监控系统状态，并清理各类开发工具与 AI Agent 留下的缓存数据。

界面支持 **中文 / English** 实时切换。

## 界面预览

<table>
  <tr>
    <td width="50%" align="center"><img src="docs/screenshots/dashboard.png" alt="仪表盘"><br><sub>仪表盘</sub></td>
    <td width="50%" align="center"><img src="docs/screenshots/deep-clean.png" alt="深度清理"><br><sub>深度清理</sub></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="docs/screenshots/smart-uninstall.png" alt="智能卸载"><br><sub>智能卸载</sub></td>
    <td width="50%" align="center"><img src="docs/screenshots/disk-analyzer.png" alt="磁盘分析"><br><sub>磁盘分析</sub></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="docs/screenshots/system-optimize.png" alt="系统优化"><br><sub>系统优化</sub></td>
    <td width="50%" align="center"><img src="docs/screenshots/live-monitor.png" alt="实时监控"><br><sub>实时监控</sub></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="docs/screenshots/project-purge.png" alt="项目清理"><br><sub>项目清理</sub></td>
    <td width="50%" align="center"><img src="docs/screenshots/installer-cleanup.png" alt="安装包清理"><br><sub>安装包清理</sub></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="docs/screenshots/docker-cleanup.png" alt="容器清理"><br><sub>容器清理</sub></td>
    <td width="50%" align="center"><img src="docs/screenshots/ai-agent-cleanup.png" alt="AI Agent 清理"><br><sub>AI Agent 清理</sub></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="docs/screenshots/menubar-quick-clean.png" alt="菜单栏 · 立即清理"><br><sub>菜单栏 · 立即清理</sub></td>
    <td width="50%" align="center"><img src="docs/screenshots/menubar-status.png" alt="菜单栏 · 系统状态"><br><sub>菜单栏 · 系统状态</sub></td>
  </tr>
</table>

## 功能模块

| 模块 | 说明 |
| --- | --- |
| **仪表盘** | 磁盘概况、可回收空间估算、权限状态一览 |
| **深度清理** | 12 个清理分类，支持二级分组勾选与风险分级（safe / caution） |
| **智能卸载** | 卸载应用并扫描其残留文件（缓存、偏好设置、日志等） |
| **磁盘分析** | 可视化分析磁盘占用，定位大文件与目录 |
| **系统优化** | 系统维护任务（DNS 刷新、索引重建、内存回收、系统文件校验等）；需要管理员权限的任务一次授权后批量执行 |
| **实时监控** | CPU、内存、磁盘、网络的实时状态与历史曲线 |
| **项目清理** | 清理开发项目的构建产物（`node_modules`、`target` 等） |
| **安装包清理** | 清理已无用的安装包（macOS `.dmg` / `.pkg`，Windows `.exe` / `.msi` / `.msix` 等） |
| **Docker 清理** | 通过 Docker Engine API 清理镜像、容器、卷与构建缓存（macOS 走 unix socket，Windows 走命名管道），无需安装 docker CLI |
| **AI Agent 清理** | 清理 Trae、Qoder、Codex、Claude Code、Gemini CLI、DSH、Comate、OpenCode、CcSwitch、Openclaw、Yuanbao、Omega 等 AI 工具留下的缓存、日志、会话数据 |
| **菜单栏 / 托盘** | 常驻菜单栏（macOS）或系统托盘（Windows）的快速清理、系统状态与网络波形 |

### 深度清理分类

用户缓存、系统缓存、应用日志、应用垃圾、浏览器缓存、废纸篓、下载目录、邮件附件、Xcode 构建数据（Windows 上为开发工具缓存）、iOS 备份、语言文件、失效符号链接。

其中应用日志覆盖 8 类来源，包括 `/Library/Logs`、`/private/var/log` 等系统运行时日志，以及 `/private/var/db/diagnostics` 下的诊断归档（Persist / Special / Signpost / HighVolume / timesync 等）与 `/Library/Logs/DiagnosticReports` 崩溃报告；Windows 侧对应 `%SystemRoot%\Logs`、`%SystemRoot%\Minidump`、`%SystemRoot%\Temp`、WER 报告与 LiveKernelReports 等系统日志位置。

## 平台差异

同一套界面与命令，各平台用符合本地习惯的方式实现：

| 能力 | macOS | Windows |
| --- | --- | --- |
| 应用卸载 | 删除 `.app` 包并扫描残留（缓存、偏好设置、日志等） | 调用厂商卸载程序（MSI / NSIS 静默参数），再清理残留的应用数据、开始菜单快捷方式与注册表项 |
| 启动项 | LaunchAgents / LaunchDaemons / 登录项 | 注册表 Run 键（HKCU / HKLM）与「启动」文件夹；可启用 / 禁用 |
| 安装包识别 | `.dmg` / `.pkg` / `.zip` / `.iso` / `.tar.gz` | `.exe` / `.msi` / `.msix` / `.appx` / `.msu` |
| 挂载镜像弹出 | `hdiutil detach` | 对虚拟光驱发送 eject IOCTL |
| Docker 连接 | `unix://` socket（`DOCKER_HOST` → `/var/run/docker.sock` → `~/.docker/run/docker.sock`） | `npipe://`（`DOCKER_HOST` → `\\.\pipe\docker_engine`） |
| 权限模型 | 完全磁盘访问权限（TCC），未授权时扫描不到受保护路径 | 无对应权限；受 UAC 保护的系统位置由提升后的令牌执行，未提升时如实提示 |
| 托盘 | 菜单栏图标 | 系统托盘图标（同样常驻、可快速清理） |
| 窗口 | 原生标题栏叠加（Overlay）与红绿灯按钮 | 无边框窗口 + 界面内自绘的最小化 / 最大化 / 关闭按钮，标题栏区域可拖拽、双击最大化 |

## 安全设计

删除操作全部经过后端白名单守卫（`ensure_removable`）：

- **前缀保护**：`/System`、`/usr`、`/private/var/db` 等 15 个敏感子树永远不可删除，只允许清空其中明确豁免的严格子路径（如 `/private/var/db/diagnostics`）；Windows 侧对应 `%SystemRoot%`、`%ProgramFiles%`、`%SystemDrive%\$Recycle.Bin` 等系统子树，同样只放行 `%SystemRoot%\Temp`、`SoftwareDistribution\Download` 等明确豁免的子路径
- **精确保护**：`~/Library`、`~/Desktop`、`/Applications` 等 32 个目录只允许清空内容、不允许整目录删除；Windows 侧对应 `%SystemDrive%\Users`、用户主目录与 `Documents` / `Desktop` / `AppData` 容器、`~\.ssh`、`~\.aws` 等凭据目录
- **符号链接解析**：删除前解析真实路径（Windows 上同时解析符号链接与 junction），防止链接指向受保护位置
- **Windows 路径归一**：所有父子关系比较都走统一的组件级比较，先剥离 canonicalize 带出的 `\\?\` 前缀、再按大小写不敏感匹配，`C:\WINDOWS\SYSTEM32\evil.dll` 无法绕过守卫
- **风险分级**：每个条目标注 safe / caution，界面按风险分组展示与默认勾选
- **操作历史**：所有清理动作记录在本地历史中，可追溯

## 技术栈

- **前端**：React 18 · TypeScript · Vite 6 · Tailwind CSS v4 · Zustand · Recharts
- **后端**：Tauri v2 · Rust（sysinfo / walkdir / rayon / tokio；Windows 侧使用 windows-rs / winreg / trash）
- **打包**：Tauri Bundler（macOS `.app` / `.dmg`，Windows NSIS `.exe`）

## 开发

环境要求：Node.js 20+、Rust 1.77.2+、对应平台的构建工具（macOS：Xcode Command Line Tools；Windows：MSVC 生成工具与 WebView2）。

```bash
npm install          # 安装前端依赖
npm run tauri dev    # 启动开发模式（Vite + Tauri）
npm run typecheck    # TypeScript 类型检查
npm run tauri build  # 构建并打包（macOS .app / .dmg，Windows NSIS 安装包）
```

Rust 侧测试：

```bash
cd src-tauri
cargo test --lib     # 运行单元测试
cargo check          # 快速编译检查
```

构建产物位于 `src-tauri/target/release/bundle/`。

### 跨平台静态检查

在 macOS 上无法运行 Windows 产物，因此用交叉编译检查 Windows 代码路径：

```bash
rustup target add x86_64-pc-windows-msvc   # 首次
node scripts/check-windows.mjs             # 等价于 cargo check --target x86_64-pc-windows-msvc --all-targets
```

Windows 完整构建由 CI 负责：`.github/workflows/windows-build.yml` 在 `windows-latest` 上执行类型检查、`cargo test`、`npm run tauri build`，并上传 NSIS 安装包（Actions artifact）。

## 权限说明

- **macOS**：部分清理目标（如 `/private/var/log`）受 TCC 保护，需在 **系统设置 → 隐私与安全性 → 完全磁盘访问权限** 中授予 Windle 权限，否则这些路径将扫描不到内容
- **Windows**：没有完全磁盘访问权限这类授权；受 UAC 保护的系统位置（`%SystemRoot%`、`%ProgramFiles%`、`%ProgramData%` 等）需要管理员权限，任务会请求提升并如实反馈结果
- 两个平台上，系统优化中的部分任务都需要管理员权限，Windle 会一次性验证并在 15 分钟内复用，避免重复弹窗

## 目录结构

```
src/                 前端源码
├── components/      界面组件（features 各功能模块 / layout / menubar / ui）
├── services/        Tauri IPC 服务封装
├── stores/          Zustand 全局状态
├── i18n/            中英文文案（含 Windows 文案覆盖项）
└── lib/             导航与工具函数
src-tauri/           Tauri / Rust 后端
├── src/commands/    命令层（clean / uninstall / optimize 等按平台拆分 macos.rs / windows.rs）
├── src/scanner/     文件系统扫描器
└── src/utils/       权限守卫、安全删除、历史记录（permissions/ 同样按平台拆分）
scripts/             跨平台静态检查脚本
```

## 更新日志

见 [CHANGELOG.md](CHANGELOG.md)。

## 许可证

[MIT](LICENSE)
