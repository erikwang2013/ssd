# 小盾 Linux 提权模型（M1e 决策落点）

© 2026 erik · erik@erik.xyz · https://erik.xyz

范围：M1 Linux 预览版如何让普通用户对物理磁盘做**只读**访问。落地件：
`packaging/udev/71-xiaodun-uaccess.rules`、`packaging/polkit/com.erik.xiaodun.policy`、
`crates/xd-daemon/src/privcheck.rs`（+ `main.rs` 接入）。

> **真机生效状态：未验证（需真机 root）**。udev 规则与 polkit 策略均为声明式文件，
> CI（无 session D-Bus、无 udevd、无 tty）无法覆盖其真实生效路径。M1 出口手测清单：
> 装包（`dpkg -i`）→ 插 U 盘 → 免密扫描成功（详见文末"未验证清单"）。

## 1. 三方案取舍

| 方案 | 内容 | 决策 |
|------|------|------|
| **A. udev uaccess（主）** | 规则对 USB 存储/SD 卡打 `TAG+="uaccess"`，systemd-logind 给**活动会话**用户加 ACL | **采用**——零代码改动、零提示、无 root 进程 |
| **C. pkexec 白名单（兜底）** | polkit action 限定 exec.path，通过 pkexec 以 root 拉起 `xd-daemon`；root 模式对参数做纵深校验 | **采用**（兜底：ACL 未命中/无活动会话时；注意 SSH/无会话场景因 `allow_any=no` 可能**硬拒且无提示**，不可用即不可用） |
| **D. AppImage 自带提权** | 打包 AppImage 并让其中二进制提权 | **不做**——结构性冲突：AppImage 走 FUSE 挂载，默认 `nosuid`，且无法安装 udev/polkit 系统文件 |

产品形态因此是 **deb + uaccess 主 / pkexec 兜底 / AppImage 不做**。

## 2. 为什么 uaccess 是 rw，而"只读"仍然成立

内核/udev 层面**没有"只读 ACL"**：`TAG+="uaccess"` 授予的是 rw ACL（`uaccess` 由
logind 翻译为 `g:user:rw`）。因此只读铁律**不在权限层**，而在两个别处：

1. **类型系统**：`xd_device::BlockDevice` 只有 `read_at`，无任何写方法，`xd-device`
   不导出写路径——恢复工具不可写盘是编译期保证。
2. **打开方式**：设备/镜像一律 `O_RDONLY`（`ImageFileDevice::open` 与
   `LinuxBlockDevice::open` 均无写打开分支）。

即：ACL 给的是"可达"，"不可写"由代码结构与 open flag 保证。审查时这两点必须同时成立。

## 3. 为什么 polkit 必须 `auth_admin`（不 keep）且 root 侧还要校验参数

**pkexec 不校验传给目标的参数**（polkit 只授权"运行这个可执行文件"，参数原样透传）。
于是存在两条攻击面：

1. **`auth_admin_keep` 或 `yes` = 本地任意文件读取**：授权被缓存后，任意以会话用户身份
   运行的代码（含被诱导执行的脚本）都能 `pkexec /usr/libexec/xiaodun/xd-daemon --image /etc/shadow`
   拿到 root 可读文件的字节，且**不再弹认证框**。`allow_any=no` + `allow_active/auth_admin`
   非 keep 把"每次调用"都钉回真人认证，这是策略层的**第一道**。
   （策略文件里的注释是**勿改警告**，不是建议。）
2. **参数纵深防御（第二道）**：`privcheck::check_image_arg` 在 euid==0 时要求
   `--image` 指向**普通文件**且**属主 == `PKEXEC_UID`**（无 `PKEXEC_UID` 拒绝）。
   即：即便策略被误改为 keep/yes，调用者也**读不到不属于自己的文件**（`/etc/shadow` 属主 root
   ≠ 调用者 → 拒绝，exit 2）。打开用 `O_NOFOLLOW`（`0o400000`），拒符号链接换靶。
   失败关闭的代价（已知边界）：无 `/proc` 可读的环境（`root_mode(None)=true`）下，非 root
   调用者也会被要求 `PKEXEC_UID`——没有该变量的普通 `--image` 调用将被拒（exit 2）。这是
   保守换安全的取舍：/proc 缺席本身是异常环境，宁可拒服务也不静默跳过校验。

`--device` 分支**不需要** euid 校验：`LinuxBlockDevice::open` 内含 `NotAFile` 拒绝，
非块设备节点（含普通文件、符号链接指向的普通文件）在打开后即被拦下。

**残留缺口（已知，未修）**：`ImageFileDevice::open` 内部按**路径**二次打开（跟随符号链接），
与上面的 `O_NOFOLLOW` 检查之间存在 TOCTOU 窗口：理论上可在检查通过后把路径换成指向他人文件的
符号链接。实际可达性受第一道约束：须诱导真人完成一次认证（`auth_admin` 给的是**每次调用的一次性
授权**，既非缓存授权，也不等于攻击者拿到 admin 口令）。**诚实边界**：残窗对已认证的执意调用者
理论可赢（在自有目录内 rename 轮换符号链接）；第二道主要防 keep 误配与非竞速攻击者，闭合归 M4
的 `from_file`（需动 `xd-device`）。

**已考虑并排除的同类面**：硬链接（`link()` 需对目标有写权限或 `Protected_hardlinks` 放行，
他人文件到不了手）；bind mount（同理性，且挂载需 CAP_SYS_ADMIN——此时攻击者已是 root）；
user namespace（userns 内的 root 无宿主 CAP_DAC_OVERRIDE，读不了宿主他人文件）。三者结论：
不构成绕过第二道的路径。

## 4. 中期硬化方向（M4 提权设计时执行）

- **root 只做最小动作**：root 进程只负责 `open()`，随即降权回调用者身份再跑解析——root 窗口
  缩到一次 open；顺带自然闭合第 3 节的 TOCTOU（后续读取都在 fd 上）。降权顺序：
  `setgroups(0)` → `setresgid` → `setresuid`（gid/uid 取 pkexec 提供的 `PKEXEC_UID` 与同源 GID，
  真机核 pkexec 是否给 GID），配上 `PR_SET_NO_NEW_PRIVS`，
  且**必须在建任何线程之前**（多线程进程降权不彻底；daemon 现为单线程入口，改动时勿破坏）。
  成本句：**降权后无法再 open** 新的设备节点（ACL 不再适用/无权限），故所有 `open` 必须前置到
  降权之前一次做完，后续解析只走已打开的 fd。
- **fd 化构造**：`ImageFileDevice` 增 `from_file(File, name)`，`privcheck` 打开的 `O_NOFOLLOW`
  fd 直接下沉，不再按路径二次打开。
- 依赖已就位：`rustix` / `libc` 已在 `Cargo.lock`（无需新增依赖即可做 setuid/openat2）。

## 5. 未验证清单（平台专有，交付标注"未验证"；M1 出口真机手测）

- [ ] udev uaccess 真机生效（两场景都要打）：①U 盘/易驱线/移动 SSD（`SUBSYSTEMS=="usb"`，
      **含 RMB=0 的硬盘盒**）②SD 卡（`mmcblk[0-9]*` + `removable==1`）；普通用户免密扫描
      （对照 `getfacl /dev/sdX1` 应见当前会话用户 `rw` ACL）。
- [ ] 内置 eMMC（`mmcblk0`，removable=0）与 `mmcblk0boot0/rpmb` **未获** uaccess（收窄逻辑
      待真机 SD 读卡器 + eMMC 双场景验证）。
- [ ] polkit 真实认证路径：本机非活动会话（`allow_inactive=auth_admin`）弹认证框；**认证一次后
      再次调用仍弹框**（证明非 keep）；SSH/无本地会话 → `allow_any=no` 硬拒（不弹框）。
- [ ] `euid==0` 参数防御：`pkexec /usr/libexec/xiaodun/xd-daemon --image <他人文件>` → exit 2
      且打印属主不符；`--image <符号链接>` → 打开失败；`--image <fifo>` → 不挂死。
- [ ] 真 U 盘删除照片全链路恢复（对真实介质，非环回）。
- [ ] 真机扫描全链路长跑（M1b 起）：真 U 盘/相机卡上启动快速扫描——观察 ①`scan.progress` 到达节奏
      （≥250ms 或 ≥1MiB 节流）②暂停后磁盘读停（iotop/`/proc/<pid>/io` 冻结）③取消后 `scan.finished
      state=canceled` 且 daemon stderr 无 panic 噪声 ④`--db` 库落 XDG 路径、重启 daemon 后结果可查
      ⑤大介质（≥32GB）进度百分比为真百分比（readBytes/totalBytes）而非假动。
- [ ] 导出降权路径（`--export-worker`，M1d T3）：本机非 root 走不到降权臂，三项真机核对见 §6 末。

## 6. 导出降权子进程模型（M1d T3）

**模型**：父 daemon（可能经 pkexec 为 root）`spawn 自身 --export-worker` →
① 子**按父权限**打开源设备 fd（降权前唯一 `open` 出口）→ ② 子在降权前**复核**目标三重校验
（权威；父侧同类校验只为同步错误码）→ ③ root 且 `PKEXEC_UID` 存在时降权到调用者 →
④ **此后所有文件写入均以普通用户身份**。父只转发子 stdout 的 JSON 行
（`progress`/`item`/`fatal`/`finished`）；子崩溃/被杀 = 导出终止，已写文件保留；取消 = SIGTERM 子进程。

顺序即安全性质：降权后**无法再 open**（§4 成本句），故所有 `open`（设备、目标目录 stat/statvfs）
必须前置到降权之前一次做完。子进程为**单线程**且在降权前不建任何线程，
故 `rustix::thread::set_thread_res_uid/-gid` + `set_thread_groups` 与进程级降权等价
（满足 §4「必须在建任何线程之前」）。顺序 `setgroups(0)` → `setresgid` → `setresuid`（反序即失败）。
库连接为 `OpenFlags::SQLITE_OPEN_READ_ONLY`（不跑迁移、不写库）。

**相对 §3 的威胁面差异与一处主动加闸**：设备 id 取自**自家任务库**（daemon 写的行），不经 RPC
参数——RPC 侧镜像只能经 `--image` 注册（`DaemonOpener` 拒 `image:`）。但常规路径下库文件属主即
调用者用户、可被其改写，故 `image:` 分支在 **root 模式复用 `--image` 同闸**
（`privcheck`：`O_NOFOLLOW` + 属主须为 `PKEXEC_UID`）。不设此闸，伪造库行 `image:/etc/shadow`
会让提权 worker 沦为任意 root 可读文件的读取器（§3 攻击 1 的另一扇门，同一类面的另一入口）。
**这是对 T3 计划「此路径无越权面」断言的偏离**（记为：论断对 RPC 层成立，对库层不成立）。
非 root 模式与原 `--image` 常规路径同语义（不做属主校验）。

**落盘名净化（qual 硬化 (a)，已修）**：同一条「库文件可被属主改写」的面还有第二个出口——
条目的 `name`/`ext` 是库里的自由文本。不净化时伪造行 `name = "../../evil"` 会让 worker 在
**目标目录之外**落物（以普通用户身份写用户自己的文件，不构成提权，但违背「导出只写 `targetDir`」
的接口承诺，且可在任意用户可写目录覆盖同名文件）。现落盘名一律过 `sanitize`（`..` → `_`；
`/`、`\`、控制字符 → `_`；净化后空/`.` → `_`），**`ext` 同样过净**——雕刻件的
`carved_{idx:06}.{ext}` 直接拼 `ext` 会重建同一条穿越串。净化后名内无分隔符，故拼接结果恒在
`targetDir` 之下；重名走 `{stem}_{N}.{ext}` 去重；`items[].name` 回传实际落盘名。
注入测试（伪造行 → 导出 → 断言目标目录外无落物、报告名无 `/`/`..`）见
`crates/xd-daemon/tests/export_ipc.rs::hostile_row_names_and_ext_cannot_escape_target_dir`。

**同盘判定（-32006）两道——盲区①已修（盘级祖先）**：
1. **精快路径**：`st_dev(target) == st_rdev(source 节点)`（一次 stat 的内核事实）。
   盲区（原明记不修、现已封）：整盘 `/dev/sdb` (8,16) 与其分区 sdb1 (8,17) 不相等 ⇒ 写回源盘
   分区不报 -32006——正撞「写错一次毁掉用户数据」铁律。
2. **盘级祖先（第二道）**：目标所在文件系统的 `st_dev` → `/sys/dev/block/<maj>:<min>`
   （canonicalize 解析软链）的路径**是否位于源设备节点路径之下**（`Path::starts_with` 按分量，
   故 `.../block/sdb/sdb1` 是 `.../block/sdb` 的后代、`sdb10` 不是）⇒ 是则 -32006。
   方向单向（目标是源的后代才拒）：源更细、目标更粗（整盘节点）放行——有分区表的整盘挂不上
   文件系统，该形态不成立。兄弟分区（sdb2 vs 源 sdb1）与另一块盘均放行：契约拦的是
   「写回源设备这条链」，不是「写回同一块物理盘」。
   `target_dev` 直接取 `stat(target).st_dev`——与 `/proc/self/mountinfo` 第 3 字段同值
   （`man proc`：「the value of st_dev for files on this filesystem」；本机 8:22/8:21 两例实测
   一致），故不另解析挂载表做最长前缀匹配（只会复现同一次 stat 的结果）。镜像源恒 `None`
   （不做同盘校验——目标是文件生态，不触碰镜像内容，双道皆免）。
3. **解析失败 fail-open + stderr 留痕**（裁定）：sysfs 节点缺（容器/无 `/sys`/匿名设备）时退回
   精快路径结论并 warn。取舍：铁律要害是「别写回源盘」，精快路径已拦最常见形态；盘级判定是
   纵深，不让环境差异挡住正常导出。对照行为：改 fail-close 则 inject 单测（空根断言）翻转。
   实现与注入根测试见 `xd-core::export::{check_on_source_at, is_descendant_at}`；真机 sysfs
   冒烟（读真 `/sys` + 真 stat，断言根 fs 设备是其整盘后代）在本机 Linux 上跑真值，
   无 sysfs 拓扑时自跳过留痕。真环回设备的 -32006 端到端断言仍归 T9/scripts（e2e-loop 挂载
   环回后导出到挂载点）。

**残留盲区（明记不修）**：父侧校验（同步错误码）与子侧复核之间的目标目录换靶 TOCTOU：子侧复核
为权威且其在降权前单线程完成，残窗仅文件系统竞争（无外部输入面）。闭合归 M4（fd 化 + `openat2`）。
**cancel 的 stale-pid 窗（qual I5 本轮记录）**：`Job.pid` 在 spawn 时定格，cancel 据此定向 `kill`；
reap 完成～state 落定之间的 µs 级窗内该 pid 已被回收，理论上可复用给无关进程而被误发 SIGTERM
（非 root 时内核按 uid 拒绝，无害；root 时本有 CAP_KILL，属误伤不属提权）。闭合归 M4（`pidfd`
或子句柄 + `try_wait` 收口）。

**T9 落地后的验证面（M1d 出口）**：`scripts/e2e-loop.sh` 的真环回段已扩到导出——
环回只读设备**挂载**后导出到挂载点断言 `-32006`（同盘判定的真块设备臂），umount 后导出到
普通目录并**逐字节**比对埋点原字节（含 `export.finished` 抢跑两序容忍）；CI 的 flutter job
跑真 daemon 全链路集成测试（扫描/分片预览/导出/报告逐字节，`XD_DAEMON_BIN` 守卫）。
以上均由非 root 用户身份执行（CI 免密 sudo 建环回，daemon 以 root 起、无 `PKEXEC_UID`
⇒ 按设计**不降权**，stderr 留痕）——即：**覆盖面止于「流程与校验」，降权臂本身仍待真机**。

**非 unix 构建边界（T9 可移植性修复链）**：Windows/macOS 编译档下，同盘校验与导出取消**显式拒绝**
（`PlatformUnsupported` → -32603 + stderr 留痕，不静默放行——「写回源盘」的防护不得无声通过）；
余量预检因无 `statvfs` 打 warn 后跳过（UX 预检非安全边界，写失败由逐件 degraded/failed 报告兜底）。
平台层真实现（`GetDiskFreeSpaceEx` 等）归 M1e-tail；CI 3 OS 矩阵已绿（含 Windows 全 workspace 测试）。

**未验证（需真机 root/pkexec）**——本机非 root，集成/单测只覆盖纯决策函数（`drop_plan`）：
- [ ] 真机 pkexec 拉起后导出：落盘文件属主 == `PKEXEC_UID` 用户（进程未降权时会是 root）。
- [ ] `setgroups(0)` 生效（`/proc/<pid>/status` 的 `Groups` 为空，无残留附加组）。
- [ ] `/etc/passwd` 无该 uid 时只降 uid 的警告路径（组属主保留 root，stderr 有留痕）。
- [ ] 真 GTK 目录选择弹窗、真桌面文件管理器「打开目标文件夹」（CI 无桌面，按计划不引入
      真 `xdg-open` 进程断言）——归 M1 出口手测。

## 7. 提权会话传输（M1e-tail T1：TCP 回环 + 令牌）

**模型**：提权（root）daemon 不再走 stdio——`--listen 127.0.0.1:0 --port-file F`：绑定回环
随机端口 → 生成会话令牌 → **原子写 F**（一行 `<port> <token>`；unix：`.tmp-<pid>` 以 `create_new`
独占 + 0600 创建后 `rename`，读者永远读不到半行）→ stderr 打印就绪。客户端（Dart
`SocketCoreClient`）读 F → `Socket.connect` 回环 → **首行必须 `{"auth":"<token>"}`**，否则
daemon 回 `-32001`（id=null）后**立即断开、不读后续**；认证通过后该连接按 stdio 同款逐行
JSON-RPC 处理（响应与通知同走一条连接）。令牌比较常数时间（长度差直接 `false`，逐字节 XOR 折叠）。
协议全文见 `crates/xd-daemon/src/transport.rs` 头注。

**威胁模型（为什么这条面成立）**：

1. **令牌文件即能力**：异用户进程读不到 0600 的 F（Linux 权限位；Windows 见未验证），读不到令牌
   即无法通过握手——回环监听面因此**不弱于**原 stdio 句柄传递：两者都要求「已是当前用户」。
   回环端口不对外网卡暴露（绑定 `127.0.0.1`），且 `main.rs` 对 `--listen` 只放行回环地址
   （非回环 → exit 2，显式拒绝而非静默）。
2. **同用户攻击者无提权增益**：同用户进程本就有 uaccess 直读设备（§1 方案 A），亦可 `ptrace`
   普通用户进程/读其内存。令牌只把「同用户的可达性」显式化，不新增任何跨用户或跨权限动作；
   本模型的收益方是**用户自己**的 UI 进程拿到 root 只读能力的通道。
3. **令牌生命周期**：daemon 启动时 16 字节 CSPRNG（unix `/dev/urandom`；windows `BCryptGenRandom`）
   → 32 位十六进制，仅存于 daemon 内存与该 0600 文件；daemon 退出即失效。M1 不做轮换——
   每次提权引导都是「新 daemon + 新令牌」（daemon 生命周期 == 会话）；轮换/过期归 M2。
4. **认证失败不泄露**：失败只有固定一条 `-32001` + 断开，不回显期望值、不区分「格式错」与
   「令牌错」，不读该连接其余内容。

**已知限制（M1 明示接受，M2 收紧）**：

- **通知广播给所有已认证连接**（非订阅路由）：daemon 侧 `Notifier` 对注册的每个写端遍历写；
  多客户端场景下 A 的扫描进度也会推给 B。M1 产品形态是「单 UI 进程 + 提权 daemon」，
  实际只有一族连接；连接路由/订阅模型归 M2。测试钉子见
  `crates/xd-daemon/tests/tcp_session.rs::notifications_broadcast_to_all_authenticated_connections`。
- **无连接数/频率限制**：本地任意进程可对回环端口反复连接、反复猜令牌。16 字节令牌空间下
  在线暴力不可行，且猜测成功者本就等价于当前用户（见威胁模型 2）；但**无速率限制本身**是
  已知缺口（连接洪泛/资源耗尽面）——M2 收紧。
- **广播持全局锁逐端写**：慢读者（TCP 缓冲写满）会拖住其余连接的广播（代码内 `ponytail:` 注释
  标记）。M1 接受；每端独立缓冲/线程归 M2。
- **stdout 仅协议**：TCP 模式下 stdout 不写任何东西（诊断全 stderr），集成测试全程断言其为空
  （`kill_and_assert_stdout_empty`）——维护 stdio 与 TCP 两模式共同的契约。
- **Windows port-file ACL 未收紧**：NTFS 无 0600 语义，现为直写（可读性由继承 ACL 决定，
  通常已限当前用户但不保证）；显式收紧仅当前用户可读 = TODO M2。
  非 unix/windows 平台**显式拒绝**（`Unsupported` / panic，不静默退化）。
  写路径本身已由 CI **实跑**（见文末「已验」），未验的只是 ACL 收紧。

**已验（CI 实跑，非仅编译）**：`rust (windows-latest)` 与 `rust (macos-latest)` 跑全 workspace
测试（含 `tcp_session.rs` 全链路：令牌生成 = Windows `BCryptGenRandom` / unix `/dev/urandom`、
port-file 写 = Windows 直写 / unix 0600+`.tmp`+rename、回环 TCP 握手与通知广播）——
CI run `37111717813`（HEAD `e309254`）5/5 job 绿。

**未验证（需真机）**：

- [ ] Windows port-file **可读性 ACL**（写路径本身已 CI 实跑）：NTFS 继承 ACL 是否恰好仅当前
      用户可读——M1 出口真机 `icacls` 核对。
- [ ] 真 UAC/osascript 提权引导拉起 TCP daemon 的端到端（Task 4 交付；CI 无桌面）。
- [ ] macOS 全盘访问（TCC）路径下的提权会话（同上）。

## 8. Windows 平台层（M1e-tail T2：SetupAPI 枚举 + 只读物理盘句柄）

**实现**：`crates/xd-device/src/windows.rs` —— `SetupDiGetClassDevsW(GUID_DEVINTERFACE_DISK)`
逐盘取接口路径 → 只读打开（desired access 仅 `GENERIC_READ`，共享 `READ|WRITE`，`OPEN_EXISTING`，
**不加** `FILE_FLAG_NO_BUFFERING`）→ `IOCTL_STORAGE_GET_DEVICE_NUMBER` 得盘号 → 路径/id
`win:\\.\PhysicalDriveN` → `IOCTL_DISK_GET_LENGTH_INFO` 取大小、`IOCTL_STORAGE_QUERY_PROPERTY
(BusType)` 映射 transport（usb/sata/nvme/other；removable = Usb/1394/Sd）→ `read_at` =
`SetFilePointerEx`+`ReadFile`（`io_lock` 串行化内核文件游标；读越尾按设备大小钳位为短读，
同 Linux pread 语义）。daemon 侧 `--device \\.\PhysicalDriveN` 走同源打开。只读铁律：全文件
无任何写 API（无 `GENERIC_WRITE`/`WriteFile`/`FILE_WRITE_DATA`）。

**已验（CI 实跑）**：`rust (windows-latest)` 实跑 `tests/windows_smoke.rs` 2/2 与
`windows::tests` 6/6 通过（枚举非空/读 boot 区/越尾钳位）——CI run `37114173261`
（HEAD `810405d`）5/5 job 绿。

**已知限制（M1 明示接受）**：

- **同盘校验缺口（-32006）**：Windows 无 `st_rdev`，`WindowsBlockDevice::source_rdev` 恒 `None`
  ⇒ 按 M1d 契约（None = 不做同盘校验，与镜像源同款），导出目标与源盘**同盘时不被拦截**。
  M1 的 Windows 用户须自行避免把恢复结果写回源盘；**Windows 目标盘同源校验 M2 补
  （卷句柄卷号比较：对目标卷与源盘各取 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 比盘号）**。
- **枚举需管理员**：`IOCTL_DISK_GET_LENGTH_INFO` 的 CTL_CODE 带 `FILE_READ_ACCESS`，且 Vista+
  上物理盘 `GENERIC_READ` 打开即需提权 ⇒ 非提权进程拿不到盘列表。M1e 本切片 daemon 侧未接
  Windows 枚举（Windows 上 `device.list` 仅含 `--image`/`--device` 注册项）；Windows daemon 由 UAC 提权
  拉起（Task 4）——非提权枚举面（0-access 句柄 + `IOCTL_STORAGE_QUERY_PROPERTY`）归 M2 评估。
- **transport/removable 仅提示**：BusType 归类不参与任何过滤（同 Linux 口径）；两平台归类可
  不一致（如 USB-SATA 桥接盘），以各自真机行为为准（见下）。

**未验证（需真机）**：

- [ ] 真机枚举/只读打开/引导区读（CI windows runner 实跑同路径冒烟：枚举 ≥1 盘、`PhysicalDrive0`
      读 512B、越尾短读——`crates/xd-device/tests/windows_smoke.rs`）；真机差异面：换盘热插拔、
      USB-SATA 桥接盘的 BusType/removable 归类、4Kn 扇区盘的 512B 读。
- [ ] UAC 提权 daemon 下 `--device` 全链路与同盘校验缺口的用户可见性（Task 4 交付）。

## 9. macOS 平台层（M1e-tail T3：/dev/diskN 枚举 + 只读打开 + Full Disk Access 提示）

**实现**：`crates/xd-device/src/macos.rs` —— `read_dir("/dev")` + 形态解析枚举整盘
（`disk<纯数字>`；分区 `diskXsY`/裸盘 `rdiskX` 不列）→ 容量经 `libc::ioctl` 的
`DKIOCGETBLOCKSIZE`(u32) × `DKIOCGETBLOCKCOUNT`(u64)（libc 未导出这两个常量 ⇒ 按 Darwin
`_IOC` 编码常量求值：`_IOR('d',24,u32)`/`_IOR('d',25,u64)`，头文件值 0x40046418/0x40086419
钉在单测）→ 打开 `/dev/diskN` 仅 `O_RDONLY`（`rustix::fs::open`；无任何写位）→ `read_at` =
`pread` 循环补齐（同 Linux 口径）。daemon 侧 `--device /dev/diskN` 走同源打开（信任边界校验：
仅整盘形态，分区/裸盘/任意路径拒绝）；FDA 缺失（EPERM）/非 operator 组（EACCES）→
`ErrorKind::PermissionDenied`，daemon 打印「需在系统设置授权完全磁盘访问」提示。
`source_rdev` 复用 `crate::source_rdev_of`（`pub(crate)`，unix 共用，`linux.rs` 同函数）。

**同盘校验（-32006）在 macOS 的语义**：精快路径 `st_dev(目标) == st_rdev(源)` 是**同式解码下的
相等比较**——`xd-core::export` 的目标侧解码同为 glibc 式（其 `major_minor` 本地副本，跨 unix
编译），故 macOS 上两侧一致、相等语义成立；解码出的数字并非 Darwin major/minor 语义，但确定性
解码不改变相等判定（若改 Darwin 解码而 xd-core 侧不改，-32006 会**静默失效**——两侧必须同式，
见 `xd-device/src/lib.rs`）。盘级祖先第二道走 sysfs，macOS 无 `/sys` ⇒ 恒不可用 → fail-open +
stderr 留痕（§6 语义），macOS 只剩精快路径。

**已验（CI 实跑）**：`rust (macos-latest)` 实跑 `tests/macos_smoke.rs` 2/2（枚举 ≥1 **硬断言**；
打开/读 512B **弱断言**——PermissionDenied 接受并 stderr 标注）与 `macos::tests` 纯函数单测
6/6 通过——CI run `37115788074`（HEAD `6304f92`）5/5 job 绿。

**已知限制（M1 明示接受）**：

- **枚举容量需权限**：`DKIOCGETBLOCK*` 需打开 fd，而 `/dev/diskN` 为 `brw-r----- root:operator`
  （普通用户 EACCES；现代 macOS 上 root 无 FDA 亦 EPERM）⇒ 普通用户上下文枚举出的盘
  `size_bytes=0`（该盘仍列入 + stderr 留痕——跳盘会让普通用户 `device.list` 全空；容量**成功
  读到 0** 的盘仍跳过）。macOS daemon 由 osascript 提权拉起（Task 4）⇒ 真机路径容量恒可查。
- **-32006「源=整盘 / 目标=其分区」盲区在 macOS 实质未封（与 Windows 缺口同级；T6 总表将列）**：
  精快路径 `st_dev(目标) == st_rdev(源)` 两侧对象不同——挂载卷的 `st_dev` 是其**分区** `dev_t`
  （如 `disk0s2`），源侧 `st_rdev` 是**整盘** `dev_t`（`disk0`）⇒ 导出到源盘自己的分区**不会被拦**；
  且 macOS 无 `/sys` ⇒ 盘级祖先第二道恒不可用（见上），无兜底。M1 明示接受，M2 以 IOKit
  盘/分区归属补。
- **transport 恒 None / removable 恒 false**：M1 不引 IOKit（计划裁定：`diskutil info -plist`
  子进程太慢），UI 分组提示在 macOS 暂缺；M2 用 IOKit 补（含 `kIOMediaRemovable`）。
- **枚举接线缺口（与 Windows 同一枚，T4 统一修）**：daemon 的 macOS `list_only` 恒空 +
  `DeviceOpener` 恒 `NoopOpener`（`main.rs` 本任务仅接 `--device` 臂）⇒ 提权 daemon 若只传
  `--listen/--port-file` 则列零设备、扫描走不通。T4 必须一并接线（枚举 → `list_only`；`unix:`
  id 的 opener 臂按平台分派；`image:` 拒绝语义保持）。

**未验证（需真机）**：

- [ ] root + 完全磁盘访问（FDA/TCC）下的真机打开/读取/扫描；FDA 缺失时 EPERM 与提示文案。
- [ ] 真机枚举盘号/容量正确性（外接 USB 盘、Apple Fabric 命名差异）；`rdisk` 性能与权限评估。
- [ ] 未签名包 + FDA 授权的 Gatekeeper 交互（归 Task 5/M2）。

## 10. 提权会话生命周期与三平台命令构造（M1e-tail T4）

**实现**：`ui/lib/core_client/elevation.dart`（命令构造纯函数 + 启动器 + 会话连接器）、
`ui/lib/features/scan/scan_controller.dart`（-32001 → 提权引导 → 换 client → 重试）、
`crates/xd-daemon/src/transport.rs::spawn_session_watchdog`（daemon 侧生命周期）、
`crates/xd-daemon/src/portfile.rs::adopt_owner_of_dir`（属主交还）。

### 10.1 三平台命令构造（逐字形态）

三者同一形态 `--listen 127.0.0.1:0 --port-file <F> --owner-pid <P>`：**字面 IP**（`localhost`
解析可能得 `::1` 或失败），端口 `0` 由内核选、实际端口写进 F（§7）。**命令面不含
`--device`/`--image`**：提权 daemon 只靠启动枚举列设备（T4 同时补上 Windows/macOS 的
`list_only` 与 `unix:`/`win:` opener 臂——§8/§9 登记的接线缺口）。理由：pkexec **不校验**
argv（§3），能少一个「任意文件路径」参数就少一个攻击面。

| 平台 | 命令 | 引用层次 |
|------|------|----------|
| Windows | `powershell.exe -NoProfile -NonInteractive -EncodedCommand <base64 UTF-16LE>`；脚本 `Start-Process -Verb RunAs -WindowStyle Hidden -FilePath '<daemon>' -ArgumentList '<args>'` | ① 脚本正文只出现在 `-EncodedCommand`（base64，无 shell 解析）；② `-FilePath`/`-ArgumentList` 用 PowerShell 单引号字面量（唯一转义 = `''`）；③ daemon 参数串内部再经 Win32 `CreateProcess` 引用（含空格/制表/引号才加引号，反斜杠仅在引号前与结尾加倍） |
| macOS | `osascript -e 'do shell script "<shell>" with administrator privileges'` | ① AppleScript 双引号字面量（`\\`/`\"`）；② shell 串经 POSIX 单引号（`'\''`）——`$`/反引号/分号在单引号内全字面化 |
| Linux | `pkexec <daemon> --listen 127.0.0.1:0 --port-file <F> --owner-pid <P>` | argv 直给（`Process.start` 不经 shell），无引用问题 |

**`-ArgumentList` 必须是单串**：PowerShell 传数组会按空格拼串（含空格路径被拆断），单串形式
才是逐字透传——KAT 钉在 `ui/test/elevation_test.dart`。**注入用例**（8 组：空格、`"`、`'`、
`$()`、反引号、`;|&>`、Windows 反斜杠尾）逐个还原断言；macOS 侧额外把 shell 串在 `/bin/sh` 上
**真跑**一遍，断言参数逐字到达且 `$(touch /tmp/pwned)`/反引号**未被求值**。

### 10.2 UI 侧生命周期（谁建、谁等、谁清）

1. 扫描页 -32001 → 对话框「需要管理员权限访问该设备」→ [授权后重试]；
2. UI 自建 **0700 会话目录**（`Directory.systemTemp.createTempSync('xiaodun-elev-')`，属主恒为
   UI 用户；**建后显式 `chmod 700`**——`createTempSync` 无 mode 参数且跟随 umask，实测 umask
   `0002` ⇒ `0775`，同组用户可 unlink/替换 `session.port`（UI 读前 race）故不能听凭 umask；
   Windows 无 POSIX 位，跳过）→ port-file 路径 `<dir>/session.port` → 构造平台计划
   （`--owner-pid` = UI 进程 pid）；
3. 起提权进程与轮询**并行**：`connectElevatedSession` 轮询 port-file（≤30s、500ms——用户可能
   在认证框上久置），读到就连接 + `ping` 握手探针；
4. 成功后：换 `_client`（新 `SocketCoreClient` 提权会话）、关旧 client、清旧会话目录、重试
   `scanStart`；失败 ⇒ 文案**「未获得授权（原因）」**+ 可重试（不静默、不悬挂）；macOS 上
   **已提权仍 -32001** ⇒ 「已提权但仍缺完全磁盘访问（系统设置 > 隐私与安全性）」（osascript
   提权 ≠ FDA，§9）。提权期间页面显示「等待授权…」并禁交互（`starting` 即 busy）。

**两种半行形态都重试**（§7 的原子写只在 unix；Windows port-file 是直写）：读到 1 段（
`FormatException`）与读到 2 段但 token 被截断（握手 -32001）都必须当作「未就绪」等下一拍；
提权器非零退出/启动失败 ⇒ **快速**拒绝（用户取消授权不必白等 30s）。退出码只作提示：
Windows `Start-Process` 在 UAC 结果之外立即返回 0，授权与否最终由「port-file 按时出现 +
握手成功」判定。

### 10.3 daemon 侧生命周期（`--owner-pid` 属主监督 + 空转自退）

UAC/osascript 提权后父进程拿不到子进程句柄（这正是 TCP 会话存在的理由），pid 是唯一可传递
的存活凭据。daemon 起 500ms tick 监督线程，两条规则（均在 `exit_cleaning` 里**清 port-file 后
`exit 0`**，stdout 保持为空）：

- **属主监督**：`--owner-pid` 给的进程消亡 ⇒ 退出（UI 崩溃/被杀不留 root 孤儿）；
  探针 unix 用 `kill(pid, 0)`（`EPERM` = 存在但跨用户 ⇒ **视为存活**，失败方向选「宁可自退不
  留孤儿」）；Windows 用 `OpenProcess(SYNCHRONIZE)` + 零超时 `WaitForSingleObject`
  （`ERROR_ACCESS_DENIED` = 存在）。**未验证（需真机 UAC 链）**。
- **空转自退**：出现过已认证连接、随后全部断开并持续 3s ⇒ 退出。同一 UI 多轮提权时，被替换的
  旧会话在客户端关闭后自清，root daemon **不累积**；启动窗口（从未连接）不触发——那由属主
  监督兜底（UI 崩在授权框上时 daemon 尚未被连过）。

`--owner-pid` 与 `--listen`/`--port-file` 的配对是硬校验：`--owner-pid` 不带会话参数 ⇒ exit 2
（不静默退 stdio）；只给 `--listen`/`--port-file` 之一 ⇒ exit 2。

### 10.4 port-file 属主交还与残留风险（T4 落地时发现的 T1 缺口）

0600 的语义是「**属主**可读」：root daemon 写的 port-file 属主 = root ⇒ UI 用户读不到令牌，
**整条提权链在此断掉**（提权成功、会话建不起来）。修法：daemon 写 port-file 时若自身 euid 为
root，把临时文件的属主/属组改成**目标目录的属主/属组**（`adopt_owner_of_dir`，紧接在原子
`rename` 前）；UI 侧对应地用自己 0700（**显式 chmod**，见 10.2）的临时目录，故交还后读者恒为
UI 用户。非提权写入
（属主 == 目录属主）不改属主，行为与 §7 一致（测试
`port_file_is_0600_and_leaves_no_temp_file` 断言这一点）。

**残留风险（M1 接受，M2 收紧）**：

- **规则而非校验**：「交给目录属主」意味着若调用方给的 port-file **父目录属主不是自己**
  （比如共享 `/tmp` 下他人预设的目录），令牌会落到那个用户手里。M1 的调用方只有本项目 UI
  （`createTempSync` 随机名 + `create_new`），风险面 = 同机其他用户**预先**用可预测路径诱导；
  M2 改为 fd 传递或校验调用者 uid，去掉「按目录属主推断」这一环。
- **交还只改属主不改目录权限**：目录权限由 UI 保证（**显式 `chmod 700`**，见 10.2——仅靠
  `createTempSync` 会跟随 umask，`0002` 下实为 0775，已修）。UI 目录若被替换/软链，交还目标
  随之变化——同样归 M2 的 fd 传递方案。

### 10.5 已知限制（M1 明示接受）

- **pid 复用窗口**：属主死后 pid 被系统复用给新进程 ⇒ 该轮 daemon 延迟自退（最长到空转自退/
  进程退出）。窗口 = pid 复用延迟，无提权增益（新进程只是被误认为属主）；M2 以句柄/`SO_PEERCRED`
  式校验替换 pid 探针。
- **换会话瞬间双 daemon**：替换旧客户端到旧 daemon 空转自退（≤3s+1 tick）之间，机器上短暂有
  两个 root daemon（各绑随机回环端口、各自 0600 令牌文件）；无跨会话能力提升。
- **Windows port-file ACL 未收紧**（同 §7）：NTFS 无 0600 语义 ⇒ 属主交还在 Windows 是 no-op。
- **UI 崩溃但会话仍活**：属主监督在 500ms tick 内收口，其间提权 daemon 仍监听回环（令牌文件
  仍在 UI 的 0700 目录里，异用户读不到）。

### 10.6 已验 / 未验证（需真机）

**已验（本地门禁 + 假 daemon/假启动器实跑；CI 由 lead 预跑）**：

- 命令构造：三平台 KAT + 8 组注入用例（macOS 侧在 `/bin/sh` 上真跑 argv）；`-EncodedCommand`
  编解码往返。
- 会话建立：真 socket + 假 daemon 全跑——成功、两种半行形态重试、超时收口（<2s）、取消快速
  拒绝（<3s）、`spawnElevation` 真起进程返回退出码。
- 扫描页：三平台 widget 流（走通换 client 并重试 `scanStart`、取消文案、macOS FDA 栏），
  含会话目录**实际 mode 断言** `mode & 0o777 == 0o700`（umask `0002` 下曾为 0775）。
- daemon 生命周期（非 root 可跑）：属主消亡 ⇒ 自退 + 清 port-file（`daemon_exits_and_cleans_
  port_file_when_owner_dies`）、末连接断开 ⇒ 空转自退（`daemon_exits_after_last_authenticated_
  connection_drops`）、`--owner-pid` 单独给出被拒（`owner_pid_without_tcp_session_is_rejected`）。
- **root 路径**（port-file 属主交还：root daemon 写 UI 目录、断言属主/0600/可读/自退/清理）：
  测试 `root_daemon_hands_port_file_back_to_owner_user` 需免密 `sudo`，**本机无免密 sudo ⇒ 本地
  跳过（eprintln 标注）**，由 CI runner（`sudo -n` 可用）实跑。

**未验证（需真机）**：

- [ ] 真 UAC 对话框 → 提权 daemon 起来的端到端（CI runner 无桌面/无交互会话）。
- [ ] 真 osascript 授权框 + 真 FDA（TCC）交互：授权后仍 EPERM 的文案路径只在 widget 层验过。
- [ ] pkexec + 已安装 polkit policy 的真实授权（本机 polkit/session D-Bus 缺位，§1 同款限制）。
- [ ] Windows port-file 可读性 ACL 与 `process_alive` 的 `OpenProcess` 语义（真机 `icacls`）。
- [ ] 真机 pid 复用窗口与双 daemon 并存窗口的可见性（需构造属主瞬时被杀）。
- [ ] **UI 侧入口缺口（T4 已记录，未修）**：Windows 非提权时枚举为空 ⇒ 首页无设备 ⇒ 用户走
      不到扫描页的 -32001 引导。M2 需在首页给提权入口（或非提权枚举面，§8）。
