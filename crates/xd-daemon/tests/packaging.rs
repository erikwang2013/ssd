// © 2026 erik · https://erik.xyz · erik@erik.xyz​‍‍​​‍​‍​‍‍‍​​‍​​‍‍​‍​​‍​‍‍​‍​‍‍​​‍​‍‍‍​​‍‍‍‍​​​​‍‍‍‍​​‍​‍‍‍‍​‍​
//! 打包线断言（M2 T10）：签名/公证链（④）+ F2 entitlements + 元数据收口（⑨）。
//!
//! 文件钉（三平台腿都跑）：
//! - `Release_entitlements_sandbox_is_false`：F2——App Sandbox 键删除、JIT entitlement 保留；
//! - `runner_rc_metadata_is_xiaodun`：Runner.rc 版本资源收口为「小盾 (Xiaodun)」；
//! - `smoke_reports_signature_state`：macOS 冒烟脚本如实打印签名状态（签/未签都过但留证据）。
//!
//! 脚本钉（`#[cfg(unix)]`，mock codesign/xcrun/security/ditto/spctl 取证，Linux/macOS 腿真跑）：
//! - `notarize_script_skips_named_without_credentials`：缺凭据 ⇒ 具名 `skip:` 行 + exit 0（绝不产半签包）；
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
fn smoke_reports_signature_state() {
    let s = read("scripts/e2e-package-smoke.sh");
    assert!(
        s.contains("codesign -dv"),
        "macOS 冒烟须如实打印签名状态（签/未签都通过但留证据）"
    );
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

    fn run_notarize(app: &Path, zip: Option<&Path>, creds: bool, path_env: &str) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("scripts/notarize.sh")).arg(app);
        if let Some(z) = zip {
            cmd.arg(z);
        }
        for v in CRED_VARS {
            cmd.env_remove(v);
        }
        if creds {
            cmd.env("APPLE_CERT_P12_BASE64", "ZmFrZQ==")
                .env("APPLE_CERT_PASSWORD", "pw")
                .env("APPLE_TEAM_ID", "TEAMID1234")
                .env("APPLE_ID", "dev@example.com")
                .env("APPLE_APP_PASSWORD", "app-pw");
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
        let out = run_notarize(Path::new("/tmp/fake.app"), None, false, &path);
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
}
