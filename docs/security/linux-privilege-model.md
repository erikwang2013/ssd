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

**未验证（需真机 root/pkexec）**——本机非 root，集成/单测只覆盖纯决策函数（`drop_plan`）：
- [ ] 真机 pkexec 拉起后导出：落盘文件属主 == `PKEXEC_UID` 用户（进程未降权时会是 root）。
- [ ] `setgroups(0)` 生效（`/proc/<pid>/status` 的 `Groups` 为空，无残留附加组）。
- [ ] `/etc/passwd` 无该 uid 时只降 uid 的警告路径（组属主保留 root，stderr 有留痕）。
- [ ] 真 GTK 目录选择弹窗、真桌面文件管理器「打开目标文件夹」（CI 无桌面，按计划不引入
      真 `xdg-open` 进程断言）——归 M1 出口手测。
