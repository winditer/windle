# 免责声明 / Disclaimer

> 最后更新：2026-10-14 · 适用于 Windle（下称「本软件」）的全部版本与全部功能
>
> Last updated: 2026-10-14 · Applies to every version and feature of Windle ("the Software")

---

## 中文

Windle 是一款系统清理与维护工具。它会在你的授权下**删除文件、卸载应用、修改启动项并执行系统优化任务**——这些操作可能不可恢复，也可能影响系统的正常运行。为避免因误删文件或错误使用产生纠纷，请在使用前仔细阅读并理解以下条款；**开始使用本软件即表示你已理解并接受本声明**。

1. **删除与修改风险**
   本软件会按你的选择删除文件（部分移入废纸篓 / 回收站，部分直接删除）、清空废纸篓 / 回收站、卸载应用及其残留数据、删除安装包、删除 Docker 镜像 / 容器 / 卷、删除 AI Agent 的缓存与会话数据、清理开发项目的构建产物等。其中部分数据一经删除**无法恢复**。误删或误清理造成的全部后果与损失，由使用者自行承担。

2. **操作前请备份**
   在执行任何清理、卸载或优化操作前，请备份重要数据（如 Time Machine、文件历史记录或第三方备份工具）。请勿在无法承受数据丢失的机器或数据上使用本软件，例如生产服务器、唯一数据副本的存储设备。

3. **自行确认每一项操作**
   本软件提供扫描结果与风险分级（safe / caution），但最终删除哪些内容由你决定。请自行核实每一项勾选内容，尤其是标记为「谨慎」（caution）的项目；请勿在未确认的情况下批量清理不了解用途的文件。

4. **权限与系统修改**
   部分功能需要完全磁盘访问权限（macOS）或管理员权限（Windows），用于扫描和清理受保护的系统位置。「系统优化」会执行 DNS 刷新、索引重建、内存回收、系统文件校验等任务，可能改变系统状态；请确认理解任务后果后再执行。

5. **第三方服务**
   容器清理通过 Docker Engine API 连接你本机的 Docker；AI Agent 清理针对第三方工具的数据目录。这些功能的行为与数据安全还取决于相应第三方软件，本软件不对第三方软件自身的问题负责。

6. **无担保**
   本软件按「现状」（AS IS）提供，不附带任何明示或暗示的担保，包括但不限于适销性、特定用途适用性与无侵权的担保；也不保证清理结果、释放空间的数额或对特定系统完全无害。

7. **责任限制**
   在法律允许的最大范围内，作者与贡献者不对任何因使用或无法使用本软件而产生的直接、间接、偶然、特殊或后果性损失负责，包括但不限于数据丢失、业务中断、系统或设备损坏。是否执行任何清理、卸载或优化操作，始终由你决定并承担相应风险。

8. **与许可证的关系**
   本声明是 [MIT 许可证](LICENSE) 中免责条款的通俗说明，不改变、不扩大也不缩减 MIT 许可证授予的权利；如本声明与许可证条款有任何冲突，以许可证为准。

9. **向他人提供本软件时**
   如果你将本软件分享或分发给他人使用，请一并提供本声明，确保对方在使用前了解上述风险与责任划分。

**如果你不同意以上任何条款，请立即停止使用并卸载本软件。**

---

## English

Windle is a system cleanup and maintenance tool. Acting on your instructions, it **deletes files, uninstalls applications, modifies startup items and runs system optimization tasks** — some of these actions cannot be undone and can affect how your system behaves. To avoid disputes caused by accidental deletions or misuse, please read and understand the terms below before using the Software. **By using the Software you acknowledge and accept this Disclaimer.**

1. **Deletion and modification risks**
   The Software deletes files according to your choices (some move to the Trash / Recycle Bin, some are removed directly), empties the Trash / Recycle Bin, uninstalls applications and their leftovers, removes installers, deletes Docker images / containers / volumes, deletes AI agent caches and session data, and cleans development build artifacts. Some of this data **cannot be recovered** once deleted. You are solely responsible for the consequences of accidental or unwanted deletions.

2. **Back up before you clean**
   Back up important data (e.g. Time Machine, File History or a third-party backup tool) before any cleanup, uninstall or optimization. Do not use the Software on machines or data that cannot afford any data loss, such as production servers or the sole copy of important files.

3. **Review every action yourself**
   The Software reports scan results with risk levels (safe / caution), but the decision of what to delete is always yours. Verify each selected item, especially anything marked "caution", and never batch-clean files whose purpose you have not confirmed.

4. **Permissions and system changes**
   Some features require Full Disk Access (macOS) or administrator rights (Windows) to scan and clean protected system locations. "System Optimize" runs tasks such as DNS flush, index rebuild, memory purge and system file verification that may change system state; make sure you understand the consequences before running them.

5. **Third-party services**
   Container cleanup talks to your local Docker through the Docker Engine API, and AI agent cleanup targets third-party tools' data directories. How those features behave also depends on the third-party software itself; the authors are not responsible for problems originating from third-party software.

6. **No warranty**
   The Software is provided "AS IS", without warranty of any kind, express or implied, including but not limited to the warranties of merchantability, fitness for a particular purpose and non-infringement. No guarantee is made about cleanup results, the amount of space reclaimed, or harmlessness on any particular system.

7. **Limitation of liability**
   To the maximum extent permitted by law, the authors and contributors are not liable for any direct, indirect, incidental, special or consequential loss arising from the use of, or inability to use, the Software, including but not limited to data loss, business interruption, or system or device damage. Whether to run any cleanup, uninstall or optimization is always your own decision, made at your own risk.

8. **Relationship to the license**
   This Disclaimer is a plain-language explanation of the warranty disclaimer in the [MIT License](LICENSE). It does not change, expand or reduce the rights granted by the MIT License; if anything here conflicts with the license, the license prevails.

9. **Sharing the Software with others**
   If you share or distribute the Software to others, provide this Disclaimer as well, so they understand these risks and the allocation of responsibility before using it.

**If you do not agree with any of the terms above, please stop using the Software and uninstall it immediately.**
