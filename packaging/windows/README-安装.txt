小盾 (xiaodun) — Windows x64 便携版
===================================

【启动】
  双击 xiaodun.exe 启动界面。引擎进程 xd-daemon.exe 必须留在同一目录
  （界面按「与主程序同目录」自动发现它；也可用环境变量 XD_DAEMON_BIN 指定别处）。

【权限说明（重要）】
  应用内的 UAC 提权引导尚未接入（计划 M2）：普通权限启动时，Windows 物理磁盘的
  枚举/读取可能拿不到结果，且本版本不会弹出 UAC 对话框。当前可用方式：
    1. 右键 xiaodun.exe →「以管理员身份运行」（该路径未在真机验证）；
    2. 或先用镜像文件体验完整流程：把 .img 路径设到环境变量 XD_IMAGE 后启动；
    3. 或手动启动引擎并显式注册设备（同时设 XD_DAEMON_BIN 指向本目录的
       xd-daemon.exe，供界面连接）：
         xd-daemon.exe --device \\.\PhysicalDrive1
  真机提权链路整体标注未验证，细节见 docs/security/linux-privilege-model.md §8/§10。

【依赖】
  Microsoft Visual C++ 运行库（VS 2015-2022 x64）。绝大多数系统已自带。

【只读承诺与同盘校验】
  扫描/预览全程对源介质只读；恢复导出必须另选目标目录。注意：Windows 的
  「目标盘是否与源设备同盘」校验尚未生效（M2 补），请自行避免把恢复结果写回源盘。

【未签名】
  本版本未做代码签名，Windows SmartScreen 可能提示 → 选择「仍要运行」。
