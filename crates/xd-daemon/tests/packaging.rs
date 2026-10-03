// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 打包线断言（M2 T10）：签名/公证链（④）+ F2 entitlements + 元数据收口（⑨）。
//!
//! 文件钉（三平台腿都跑）：
//! - `Release_entitlements_sandbox_is_false`：F2——App Sandbox 键删除、JIT entitlement 保留；
//! - `runner_rc_metadata_is_xiaodun`：Runner.rc 版本资源收口为「小盾 (Xiaodun)」；
//! - `smoke_reports_signature_state`：macOS 冒烟脚本如实打印签名状态（签/未签都过但留证据）；
//! - `shell_scripts_avoid_unbraced_var_adjacent_to_non_ascii`：全仓 tracked `*.sh` 里 `$var`
//!   后紧邻非 ASCII 字节 ⇒ macOS bash 3.2 并名 + `set -u` unbound（CI 二红真根因）的静态钉；
//! - `ci_packaging_macos_matrix_two_legs_and_secret_injection`（T11）：ci.yml 的 package-macos
//!   矩阵两腿（macos-15/arm64、macos-15-intel/x64 成对）+ `fail-fast: false` + artifact 名带
//!   `${{ matrix.arch }}`（旧名 `xiaodun-macos-zip` 不得回归）+ job 级 secrets env 注入
//!   （Apple 五件套 / Windows 两件套，空串语义由 notarize.sh `add_missing` 承接）。
//!   T11 只能以 dispatch 实证两腿，CI 改动此前无任何本地门禁——本钉即那门禁。
//!
//! 脚本钉（`#[cfg(unix)]`，mock codesign/xcrun/security/ditto/spctl 取证，Linux/macOS 腿真跑）：
//! - `notarize_script_skips_named_without_credentials`：缺凭据 ⇒ 具名 `skip:` 行 + exit 0（绝不产半签包）；
//! - `notarize_script_skips_named_when_credentials_are_empty_strings`（T11）：五件套**空串**
//!   （CI job 级 env 注入未配置 secrets 的真形态）⇒ 同样具名 skip + exit 0——门控须判空非判存在；
//! - `notarize_script_orders_nested_sign_first`：逐嵌套签名顺序 = 引擎 → Frameworks 内层 → framework
//!   目录 → .app；无 `--deep`；顺序 sign → zip → submit → staple → spctl，且每次签名都带
//!   `--force --options runtime --timestamp`（计划 T10 Step 1 的机器断言）。
//!
//! **未验证（需真证书 + 真机/runner secrets）**：真 notarytool 提交/公证通过、真 spctl/Gatekeeper
//! 首开、Windows signtool 真签名与 SmartScreen——见计划 T10「测试清单」末句与 README。

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {} 失败: {e}", p.display()))
}

#[test]
#[allow(non_snake_case)] // 计划测试清单用名，保留原文便于对照（非 Rust 惯例命名）
fn Release_entitlements_sandbox_is_false() {
    let release = read("ui/macos/Runner/Release.entitlements");
    assert!(
        !release.contains("com.apple.security.app-sandbox"),
        "F2: Release.entitlements 必须删除 app-sandbox 键（数据恢复与沙箱不兼容）"
    );
    assert!(
        release.contains("com.apple.security.cs.allow-jit"),
        "F2: Flutter 运行时 JIT entitlement 必须保留"
    );
    let debug = read("ui/macos/Runner/DebugProfile.entitlements");
    assert!(
        !debug.contains("com.apple.security.app-sandbox"),
        "F2: DebugProfile 与 Release 对齐（沙箱键同样删除）"
    );
    assert!(
        debug.contains("com.apple.security.cs.allow-jit"),
        "F2: DebugProfile 保留 JIT（Flutter 调试运行时）"
    );
}

#[test]
fn runner_rc_metadata_is_xiaodun() {
    let rc = read("ui/windows/runner/Runner.rc");
    for needle in [
        r#"VALUE "CompanyName", "Xiaodun""#,
        r#"VALUE "FileDescription", "小盾 (Xiaodun)""#,
        r#"VALUE "ProductName", "小盾 (Xiaodun)""#,
        r#"VALUE "OriginalFilename", "xiaodun.exe""#,
        r#"VALUE "InternalName", "xiaodun""#,
        r#"VALUE "LegalCopyright", "© 2026 erik · https://erik.xyz""#,
    ] {
        assert!(rc.contains(needle), "Runner.rc 缺条目: {needle}");
    }
    assert!(
        !rc.contains("com.example"),
        "⑨ 收口: Runner.rc 不得残留 com.example"
    );
    assert!(
        rc.contains("#pragma code_page(65001)"),
        "非 ASCII 元数据（小盾/©/·）依赖 UTF-8 code_page 头部被 rc.exe 正确解析"
    );
}

#[test]
fn macos_bundle_metadata_is_xiaodun() {
    let plist = read("ui/macos/Runner/Info.plist");
    for needle in [
        "<string>小盾 (Xiaodun)</string>", // CFBundleDisplayName
        "<string>小盾</string>",           // CFBundleName
        "<string>© 2026 erik · https://erik.xyz</string>", // NSHumanReadableCopyright
        "<string>$(PRODUCT_BUNDLE_IDENTIFIER)</string>", // 标识符仍由 xcconfig 注入
    ] {
        assert!(plist.contains(needle), "Info.plist 缺条目: {needle}");
    }
    let xcc = read("ui/macos/Runner/Configs/AppInfo.xcconfig");
    assert!(
        xcc.contains("PRODUCT_BUNDLE_IDENTIFIER = com.erik.xiaodun"),
        "AppInfo.xcconfig 的 bundle id 必须是 com.erik.xiaodun（TCC/FDA 授权按 bundle id 归属，改了就丢授权）"
    );
    assert!(
        !plist.contains("com.example") && !xcc.contains("com.example"),
        "⑨ 收口: macOS 元数据不得残留 com.example"
    );
}

#[test]
fn package_macos_wires_sign_then_rezip() {
    let s = read("scripts/package-macos.sh");
    let lines: Vec<&str> = s.lines().collect();
    let find = |pred: &dyn Fn(&str) -> bool, what: &str| {
        lines
            .iter()
            .position(|l| pred(l))
            .unwrap_or_else(|| panic!("package-macos.sh 缺「{what}」"))
    };
    let i_install = find(
        &|l| {
            l.trim_start()
                .starts_with("install -m 755 target/release/xd-daemon")
        },
        "引擎安装行",
    );
    let i_notarize = find(
        &|l| l.trim_start().starts_with("bash scripts/notarize.sh"),
        "notarize 调用",
    );
    assert!(
        lines[i_notarize].contains("xiaodun_ui.app") && lines[i_notarize].contains("$out"),
        "notarize 调用须带 .app 与 zip 两个参数: {}",
        lines[i_notarize]
    );
    let i_reditto = lines
        .iter()
        .rposition(|l| {
            l.trim_start()
                .starts_with("ditto -c -k --keepParent \"$stage\" \"$out\"")
        })
        .unwrap_or_else(|| panic!("package-macos.sh 缺 staple 后重出的交付 zip ditto"));
    assert!(
        i_install < i_notarize,
        "引擎必须先在 Contents/MacOS 就位再签名（先签后装会破坏封条）"
    );
    assert!(
        i_notarize < i_reditto,
        "交付 zip 必须在 notarize/staple 之后重出（否则包内 .app 与已装订票据不一致）"
    );
}

#[test]
fn smoke_reports_signature_state() {
    let s = read("scripts/e2e-package-smoke.sh");
    assert!(
        s.lines()
            .any(|l| l.trim_start().starts_with("codesign -dv")),
        "macOS 冒烟须有一行可执行的 codesign -dv（注释不算）如实打印签名状态"
    );
}

#[test]
fn notarize_script_avoids_bash4_only_constructs() {
    // macOS runner 的 `bash` = /bin/bash 3.2。本测试把「只用 bash 3.2 语法」钉住：
    // 全行注释先剔除（注释里正当解释被禁形态），其余代码不允许出现 4.x-only 构造。
    let s = read("scripts/notarize.sh");
    let code: String = s
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for bad in [
        "${!",
        "mapfile",
        "readarray",
        "declare -A",
        "&>",
        "|&",
        ";;&",
    ] {
        assert!(
            !code.contains(bad),
            "bash 3.2（macOS /bin/bash）不支持 {bad}——notarize.sh 须在 CI runner 默认 shell 下可跑"
        );
    }
}

/// `$var` 后紧邻非 ASCII 字节（全角标点等，UTF-8 首字节 ≥0x80）的静态钉。
///
/// 真根因（CI macOS 腿二红实证，2026-10-03）：macOS bash 3.2 会把紧邻的 0xEF 并入变量名
/// （`$missing；` ⇒ 名字 `missing\xEF`）⇒ `set -u` 下 unbound，报错行 = 打印 skip 的那行。
/// 本机 bash 5（任意 locale）复现不出，只有静态扫描能确定性拦——花括号定界 `${var}` 的 `}`
/// 在任何 bash/locale 下都终止名字解析。手写字节扫描（含 heredoc；整行注释不展开故剔除）；
/// `${...}`/`$(...)`/`$'...'`/`$?` 等天然不误报，`\$` 跳过。
#[test]
fn shell_scripts_avoid_unbraced_var_adjacent_to_non_ascii() {
    let root = repo_root();
    let out = std::process::Command::new("git")
        .args(["ls-files", "*.sh"])
        .current_dir(&root)
        .output()
        .expect("跑 git ls-files 失败（本测试需要 git 与仓库检出）");
    assert!(out.status.success(), "git ls-files '*.sh' 失败");
    let listing = String::from_utf8(out.stdout).unwrap();
    assert!(!listing.trim().is_empty(), "git ls-files '*.sh' 无输出");

    let mut offenders = Vec::new();
    for rel in listing.lines() {
        let bytes = std::fs::read(root.join(rel)).unwrap();
        for (ln, raw) in bytes.split(|b| *b == b'\n').enumerate() {
            if raw.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'#') {
                continue; // 整行注释：bash 不展开
            }
            let mut i = 0;
            while i < raw.len() {
                if raw[i] != b'$' {
                    i += 1;
                    continue;
                }
                let mut backslashes = 0;
                while i > backslashes && raw[i - 1 - backslashes] == b'\\' {
                    backslashes += 1;
                }
                if backslashes % 2 == 1 {
                    i += 1; // 反斜杠转义的 `\$` 不展开
                    continue;
                }
                let start = i + 1;
                let mut m = start;
                if m < raw.len() && (raw[m].is_ascii_alphabetic() || raw[m] == b'_') {
                    m += 1;
                    while m < raw.len() && (raw[m].is_ascii_alphanumeric() || raw[m] == b'_') {
                        m += 1;
                    }
                    if m < raw.len() && raw[m] >= 0x80 {
                        let name = String::from_utf8_lossy(&raw[start..m]).into_owned();
                        offenders.push(format!(
                            "{rel}:{}: ${name} 后紧邻非 ASCII 字节 0x{:02X} ⇒ 写 ${{{name}}}",
                            ln + 1,
                            raw[m]
                        ));
                    }
                }
                i = if m > start { m } else { i + 1 };
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "macOS bash 3.2 会把 `$var` 后紧邻的非 ASCII 字节并入变量名 ⇒ `set -u` 下 unbound（CI 二红真根因）；改用花括号定界：\n{}",
        offenders.join("\n")
    );
}

/// ci.yml 中 `job` 块（缩进 2 的 `  job:` 头 → 下一个同级 job 头或 EOF）的代码行（剔整行注释）。
///
/// 必须剔除注释：本仓 ci.yml 的注释里天然写着 `macos-15`/`macos-15-intel`/旧 artifact 名，
/// 直接对全文 `contains` 断言会被注释骗过（删了真腿照样绿）——结构断言必须落在代码行上。
fn ci_job_code_lines<'a>(ci: &'a str, job: &str) -> Vec<&'a str> {
    let header = format!("  {job}:");
    let mut inside = false;
    let mut out = Vec::new();
    for l in ci.lines() {
        if !inside {
            inside = l.trim_end() == header;
            continue;
        }
        // 下一个 job 头（缩进恰 2 个空格、非注释、以 ':' 结尾）⇒ 块结束。
        if l.starts_with("  ")
            && !l.starts_with("   ")
            && l.trim_end().ends_with(':')
            && !l.trim_start().starts_with('#')
        {
            break;
        }
        if !l.trim_start().starts_with('#') {
            out.push(l);
        }
    }
    assert!(inside, "ci.yml 缺 job `{job}`");
    assert!(
        !out.is_empty(),
        "ci.yml job `{job}` 代码行为空（解析假设失效）"
    );
    out
}

/// `steps:` 下的 run 行（序列项 `- run: <cmd>`；容忍 `- ` 前缀）。
fn is_run_step(l: &str, cmd: &str) -> bool {
    l.trim().trim_start_matches("- ").trim() == format!("run: {cmd}")
}

/// 从 matrix `include:` 段抽 (runner, arch) 成对腿——钉「runner↔arch 配对」，防换 runner 不换 arch。
fn matrix_legs(code: &[&str]) -> Vec<(String, String)> {
    let mut legs: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    let val = |t: &str, prefix: &str| {
        t.strip_prefix(prefix)
            .map(|v| v.split('#').next().unwrap_or("").trim().to_string())
    };
    for l in code {
        let t = l.trim();
        if let Some(r) = val(t, "- runner:") {
            if let Some(p) = cur.take() {
                legs.push(p);
            }
            cur = Some((r, String::new()));
        } else if let Some(a) = val(t, "arch:")
            && let Some((r, _)) = cur.take()
        {
            cur = Some((r, a));
        }
    }
    if let Some(p) = cur {
        legs.push(p);
    }
    legs
}

#[test]
fn ci_packaging_macos_matrix_two_legs_and_secret_injection() {
    let ci = read(".github/workflows/ci.yml");
    let macos = ci_job_code_lines(&ci, "package-macos");

    assert!(
        macos
            .iter()
            .any(|l| l.trim() == "if: github.event.inputs.packaging == 'true'"),
        "package-macos 必须保持 workflow_dispatch 手动门控（packaging=true）"
    );

    // 两腿 runner↔arch 成对：删腿 / 换 runner / 换 arch 任一回归都红（矩阵无序，按集合比）。
    let mut legs = matrix_legs(&macos);
    legs.sort();
    assert_eq!(
        legs,
        vec![
            ("macos-15".to_string(), "arm64".to_string()),
            ("macos-15-intel".to_string(), "x64".to_string()),
        ],
        "package-macos 矩阵须为 macos-15/arm64 + macos-15-intel/x64 两腿（计划 R7：x64 物证 = Intel runner 真跑）"
    );
    assert!(
        macos.iter().any(|l| l.trim() == "fail-fast: false"),
        "矩阵须 fail-fast: false——一腿红不得吞另一腿的物证"
    );
    assert!(
        macos
            .iter()
            .any(|l| l.trim() == "runs-on: ${{ matrix.runner }}"),
        "runs-on 须走 ${{ matrix.runner }}（不得回退单 runner）"
    );

    // artifact：arch 变量命名；旧名不得回归（T11 裁定，README/§11 同口径）。
    assert!(
        macos
            .iter()
            .any(|l| l.trim() == "name: xiaodun-macos-${{ matrix.arch }}-zip"),
        "artifact 名须为 xiaodun-macos-${{ matrix.arch }}-zip"
    );
    assert!(
        !macos.iter().any(|l| l.trim() == "name: xiaodun-macos-zip"),
        "T11 裁定旧名 xiaodun-macos-zip 不保留兼容映射（README 打包节）"
    );

    // 两腿各自跑打包 + 产物冒烟（x64 腿绿的物证前提：包内 daemon 在本腿真 exec）。
    assert!(
        macos
            .iter()
            .any(|l| is_run_step(l, "bash scripts/package-macos.sh")),
        "package-macos 须跑打包脚本"
    );
    assert!(
        macos
            .iter()
            .any(|l| is_run_step(l, "bash scripts/e2e-package-smoke.sh")),
        "package-macos 两腿须各自跑产物冒烟"
    );

    // job 级 secrets env 注入（Apple 五件套）：缩进恰 6 = 直属 job 的 `env:`（step 级 env 更深）；
    // 未配置 = 空串 ⇒ notarize.sh `add_missing` 逐件判空走具名 skip。
    for v in [
        "APPLE_CERT_P12_BASE64",
        "APPLE_CERT_PASSWORD",
        "APPLE_TEAM_ID",
        "APPLE_ID",
        "APPLE_APP_PASSWORD",
    ] {
        let needle = format!("{v}: ${{{{ secrets.{v} }}}}");
        let l = macos
            .iter()
            .find(|l| l.trim() == needle)
            .unwrap_or_else(|| panic!("package-macos 缺 job 级 env 注入: {needle}"));
        let indent = l.len() - l.trim_start().len();
        assert_eq!(indent, 6, "{v} 须在 job 级 env（缩进 6）注入: {l}");
    }

    // Windows 腿：job 级 env 两键仍在（T11 未动名/未删；空串 ⇒ package-windows.ps1 `-not` skip）。
    let win = ci_job_code_lines(&ci, "package-windows");
    for v in ["WINDOWS_CERT_PFX_BASE64", "WINDOWS_CERT_PASSWORD"] {
        let needle = format!("{v}: ${{{{ secrets.{v} }}}}");
        let l = win
            .iter()
            .find(|l| l.trim() == needle)
            .unwrap_or_else(|| panic!("package-windows 缺 env 注入: {needle}"));
        let indent = l.len() - l.trim_start().len();
        assert_eq!(indent, 6, "{v} 须在 job 级 env（缩进 6）注入: {l}");
    }
}

#[cfg(unix)]
mod scripts {
    use super::repo_root;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};

    const CRED_VARS: [&str; 6] = [
        "APPLE_CERT_P12_BASE64",
        "APPLE_CERT_PASSWORD",
        "APPLE_TEAM_ID",
        "APPLE_ID",
        "APPLE_APP_PASSWORD",
        "APPLE_SIGN_IDENTITY",
    ];

    /// 凭据注入形态：`None` = 未设置（本地开发）；`Empty` = 设置为空串（T11 起 CI 真形态，
    /// job 级 `env:` 注入未配置 secrets = 空串）。「齐备」形态由 mock 链测试自建命令。
    enum Creds {
        None,
        Empty,
    }

    fn run_notarize(app: &Path, zip: Option<&Path>, creds: Creds, path_env: &str) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("scripts/notarize.sh")).arg(app);
        if let Some(z) = zip {
            cmd.arg(z);
        }
        for v in CRED_VARS {
            cmd.env_remove(v);
        }
        match creds {
            Creds::None => {}
            Creds::Empty => {
                for v in &CRED_VARS[..5] {
                    cmd.env(v, "");
                }
            }
        }
        cmd.env("PATH", path_env)
            .current_dir(repo_root())
            .output()
            .unwrap()
    }

    fn write_mock(bin: &Path, name: &str, extra: &str) {
        let body = format!("#!/bin/sh\nprintf '%s\\n' \"{name} $*\" >> \"$MOCK_LOG\"\n{extra}");
        let p = bin.join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn touch_last_arg() -> &'static str {
        // 便携取末参（sh 无 ${@: -1}）：ditto/ cp 目标即末参。
        "for last; do :; done\n: > \"$last\"\n"
    }

    #[test]
    fn notarize_script_skips_named_without_credentials() {
        let path = std::env::var("PATH").unwrap();
        // 计划 T10 Step 6 的字面命令形态：单参数（zip 可省）也必须 skip + exit 0。
        let out = run_notarize(Path::new("/tmp/fake.app"), None, Creds::None, &path);
        assert!(
            out.status.success(),
            "缺凭据必须 exit 0（绝不产半签包）；stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("skip:"),
            "须打印具名 skip 行；实际: {stdout}"
        );
        assert!(
            stdout.contains("APPLE_CERT_P12_BASE64"),
            "skip 行须具名缺哪个凭据；实际: {stdout}"
        );
        assert!(
            stdout.contains("未签名") && stdout.contains("未公证"),
            "跳过时须如实声明本产物未签名/未公证；实际: {stdout}"
        );
    }

    #[test]
    fn notarize_script_skips_named_when_credentials_are_empty_strings() {
        // T11 新增不变量：ci.yml 在 job 级 env 注入五件套，未配置的 secrets = **空串**
        // （不是未设置——GitHub 官方语义：unset secret 的表达式值为空串）。凭据门控必须
        // 「判空」而非「判存在」：存在性判定（`${VAR+x}` 形态）在空串下会误判「凭据齐备」，
        // 进而 base64 -d 空输入 / notarytool 空凭据中途硬失败。本钉把 CI 真形态钉死。
        let out = run_notarize(
            Path::new("/tmp/fake.app"),
            None,
            Creds::Empty,
            &std::env::var("PATH").unwrap(),
        );
        assert!(
            out.status.success(),
            "五件套全为空串必须 exit 0（绝不产半签包）；stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("skip:"),
            "空串形态须走具名 skip；实际: {stdout}"
        );
        for v in &CRED_VARS[..5] {
            assert!(
                stdout.contains(v),
                "skip 行须具名全部五个缺失凭据（含 {v}）；实际: {stdout}"
            );
        }
        assert!(
            stdout.contains("未签名") && stdout.contains("未公证"),
            "空串跳过时须如实声明未签名/未公证；实际: {stdout}"
        );
    }

    #[test]
    fn notarize_script_orders_nested_sign_first() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("stage/xiaodun_ui.app");
        let daemon = app.join("Contents/MacOS/xd-daemon");
        let fw_exe = app.join("Contents/Frameworks/FlutterMacOS.framework/Versions/A/FlutterMacOS");
        for f in [&daemon, &app.join("Contents/MacOS/xiaodun_ui"), &fw_exe] {
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, b"fake mach-o").unwrap();
            // Frameworks 内层可执行靠 `find -perm -u+x` 命中——假体也要可执行位。
            fs::set_permissions(f, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let bin = tmp.path().join("mock-bin");
        fs::create_dir_all(&bin).unwrap();
        let log = tmp.path().join("calls.log");
        write_mock(&bin, "codesign", "");
        write_mock(&bin, "xcrun", "");
        write_mock(&bin, "spctl", "");
        write_mock(&bin, "ditto", touch_last_arg());
        write_mock(
            &bin,
            "security",
            "if [ \"$1\" = \"find-identity\" ]; then\n\
             \x20 printf '%s\\n' '  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Developer ID Application: Mock (TEAMID)\"'\n\
             fi\n",
        );

        let zip = tmp.path().join("dist/xiaodun-mock.zip");
        fs::create_dir_all(zip.parent().unwrap()).unwrap();
        let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("scripts/notarize.sh"))
            .arg(&app)
            .arg(&zip)
            .env("MOCK_LOG", &log)
            .env("PATH", &path_env)
            .current_dir(repo_root());
        for v in CRED_VARS {
            cmd.env_remove(v);
        }
        cmd.env("APPLE_CERT_P12_BASE64", "ZmFrZQ==")
            .env("APPLE_CERT_PASSWORD", "pw")
            .env("APPLE_TEAM_ID", "TEAMID1234")
            .env("APPLE_ID", "dev@example.com")
            .env("APPLE_APP_PASSWORD", "app-pw");
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "有凭据 + mock 工具路径应成功；stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );

        let text = fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let pos = |what: &str, f: &dyn Fn(&str) -> bool| {
            lines
                .iter()
                .position(|l| f(l))
                .unwrap_or_else(|| panic!("日志缺「{what}」:\n{text}"))
        };
        let app_s = app.to_str().unwrap();
        let p_daemon = pos("引擎签名", &|l| {
            l.starts_with("codesign ")
                && l.contains(" --sign ")
                && l.ends_with("Contents/MacOS/xd-daemon")
        });
        let p_fw_exe = pos("framework 内层签名", &|l| {
            l.starts_with("codesign ") && l.contains(" --sign ") && l.ends_with("FlutterMacOS")
        });
        let p_fw_dir = pos("framework 目录签名", &|l| {
            l.starts_with("codesign ")
                && l.contains(" --sign ")
                && l.ends_with("FlutterMacOS.framework")
        });
        let p_app = pos(".app 整包签名", &|l| {
            l.starts_with("codesign ") && l.contains(" --sign ") && l.ends_with(app_s)
        });
        let p_verify = pos("seal 断言", &|l| {
            l.starts_with("codesign --verify --strict")
        });
        let p_zip = pos("提交用 zip", &|l| l.starts_with("ditto -c -k"));
        let p_submit = pos("notarytool 提交", &|l| {
            l.starts_with("xcrun notarytool submit") && l.contains("--wait")
        });
        let p_staple = pos("staple", &|l| l.starts_with("xcrun stapler staple"));
        let p_spctl = pos("spctl 断言", &|l| l.starts_with("spctl -a -vv"));

        assert!(p_daemon < p_fw_exe, "引擎须先于 Frameworks 内层签名");
        assert!(
            p_fw_exe < p_fw_dir,
            "framework 内层可执行须先于 framework 目录签名"
        );
        assert!(p_fw_dir < p_app, "Frameworks 须先于整个 .app 签名");
        assert!(p_app < p_verify, "seal 断言须在全部签名之后");
        assert!(p_verify < p_zip, "官方顺序: sign → zip → submit");
        assert!(p_zip < p_submit, "提交的 zip 须先于 submit 产出");
        assert!(p_submit < p_staple, "staple 须在公证提交之后");
        assert!(p_staple < p_spctl, "spctl 断言须在 staple 之后");
        assert!(!text.contains("--deep"), "禁用 --deep（计划 T10 Step 1）");

        let signs: Vec<&&str> = lines
            .iter()
            .filter(|l| l.starts_with("codesign ") && l.contains(" --sign "))
            .collect();
        assert!(
            signs.len() >= 4,
            "至少 4 次签名（引擎/内层/目录/整包）: {text}"
        );
        for l in signs {
            for flag in ["--force", "--options runtime", "--timestamp"] {
                assert!(l.contains(flag), "签名调用缺 {flag}: {l}");
            }
        }
    }

    #[test]
    fn notarize_script_fails_fast_on_sign_failure_and_cleans_up() {
        // codesign 失败必须通过 set -e 立刻传播：不得继续签 .app、不得提交公证/装订；
        // 且 EXIT trap 在失败路径也要清掉临时 keychain（私钥）。
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("stage/xiaodun_ui.app");
        let daemon = app.join("Contents/MacOS/xd-daemon");
        fs::create_dir_all(daemon.parent().unwrap()).unwrap();
        fs::write(&daemon, b"fake mach-o").unwrap();
        fs::set_permissions(&daemon, fs::Permissions::from_mode(0o755)).unwrap();

        let bin = tmp.path().join("mock-bin");
        fs::create_dir_all(&bin).unwrap();
        let log = tmp.path().join("calls.log");
        write_mock(&bin, "codesign", "exit 1\n"); // 首个签名即失败
        write_mock(&bin, "xcrun", "");
        write_mock(&bin, "spctl", "");
        write_mock(&bin, "ditto", touch_last_arg());
        write_mock(
            &bin,
            "security",
            "if [ \"$1\" = \"find-identity\" ]; then\n\
             \x20 printf '%s\\n' '  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Developer ID Application: Mock (TEAMID)\"'\n\
             fi\n",
        );

        let zip = tmp.path().join("dist/out.zip");
        fs::create_dir_all(zip.parent().unwrap()).unwrap();
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("scripts/notarize.sh"))
            .arg(&app)
            .arg(&zip)
            .env("MOCK_LOG", &log)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .current_dir(repo_root());
        for v in CRED_VARS {
            cmd.env_remove(v);
        }
        cmd.env("APPLE_CERT_P12_BASE64", "ZmFrZQ==")
            .env("APPLE_CERT_PASSWORD", "pw")
            .env("APPLE_TEAM_ID", "TEAMID1234")
            .env("APPLE_ID", "dev@example.com")
            .env("APPLE_APP_PASSWORD", "app-pw");
        let out = cmd.output().unwrap();
        assert!(
            !out.status.success(),
            "codesign 失败必须传播为非零退出；stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let text = fs::read_to_string(&log).unwrap();
        let signs = text
            .lines()
            .filter(|l| l.starts_with("codesign ") && l.contains(" --sign "))
            .count();
        assert_eq!(
            signs, 1,
            "首个签名失败后必须立刻停止（不得继续签 .app）;日志:\n{text}"
        );
        assert!(
            !text.contains("notarytool"),
            "签名失败后不得提交公证;日志:\n{text}"
        );
        assert!(
            !text.contains("stapler"),
            "签名失败后不得 staple;日志:\n{text}"
        );
        assert!(
            text.contains("security delete-keychain"),
            "EXIT trap 在失败路径也必须清理临时 keychain;日志:\n{text}"
        );
    }

    #[test]
    fn notarize_script_skip_is_inert() {
        // 「绝不产半签包」的另一半：凭据缺失时不得触碰 .app、不得产出 zip。
        // app 传文件（非目录）即可——skip 门控先于一切校验，任何触碰都会改 mtime/内容。
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("fake.app");
        fs::write(&app, b"untouched").unwrap();
        let before_mtime = fs::metadata(&app).unwrap().modified().unwrap();
        let zip = tmp.path().join("dist/out.zip");
        let out = run_notarize(
            &app,
            Some(&zip),
            Creds::None,
            &std::env::var("PATH").unwrap(),
        );
        assert!(out.status.success(), "skip 路径必须 exit 0");
        assert!(!zip.exists(), "skip 不得产出 zip（绝不产半签包）");
        assert_eq!(
            fs::read(&app).unwrap(),
            b"untouched",
            "skip 不得改写 .app 内容"
        );
        assert_eq!(
            fs::metadata(&app).unwrap().modified().unwrap(),
            before_mtime,
            "skip 不得触碰 .app（mtime 变了）"
        );
    }
}
