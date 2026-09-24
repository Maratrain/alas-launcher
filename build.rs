use std::{env, fs, path::PathBuf};

use base64::{prelude::BASE64_STANDARD, Engine};

const MTLS_IDENTITY_ENV: &str = "ALAS_LAUNCHER_MTLS_IDENTITY_PEM_B64";
const REQUIRE_MTLS_ENV: &str = "REQUIRE_LAUNCHER_MTLS_IDENTITY";
const LAUNCHER_UPDATE_URL_ENV: &str = "LAUNCHER_UPDATE_URL";
// 启动器自更新清单地址。指向自有仓库的 GitHub Release。
// releases/latest/download/ 的语义是「最新正式发布的那个版本」，URL 永久不变，
// 而清单内容随每个 tag 更新 —— 正好满足「编译进二进制的地址必须固定」的要求。
// 每次发版必须确保 stable.json 已作为 release 资产上传，否则这里会 404。
const DEFAULT_LAUNCHER_UPDATE_URL: &str =
    "https://github.com/Maratrain/alas-launcher/releases/latest/download/stable.json";

fn main() {
    let windows = tauri_build::WindowsAttributes::new().app_manifest(
        r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#,
    );
    let attrs = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attrs).expect("failed to run tauri build script");

    check_version_consistency();

    // Ensure icons directory is watched for changes
    println!("cargo:rerun-if-changed=icons/");
    println!("cargo:rerun-if-env-changed={MTLS_IDENTITY_ENV}");
    println!("cargo:rerun-if-env-changed={REQUIRE_MTLS_ENV}");
    println!("cargo:rerun-if-env-changed={LAUNCHER_UPDATE_URL_ENV}");

    let launcher_update_url = env::var(LAUNCHER_UPDATE_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LAUNCHER_UPDATE_URL.to_string());
    println!("cargo:rustc-env={LAUNCHER_UPDATE_URL_ENV}={launcher_update_url}");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR is set by Cargo");
    let out_dir = PathBuf::from(out_dir);

    let bootstrap_uv_target = out_dir.join("bootstrap_uv.bin");
    if let Ok(source) = env::var("ALAS_BOOTSTRAP_UV") {
        fs::copy(&source, &bootstrap_uv_target).expect("copy ALAS_BOOTSTRAP_UV");
        println!("cargo:rerun-if-env-changed=ALAS_BOOTSTRAP_UV");
        println!("cargo:rerun-if-changed={source}");
    } else {
        fs::write(&bootstrap_uv_target, []).expect("write empty bootstrap uv placeholder");
        println!("cargo:warning=ALAS_BOOTSTRAP_UV is not set; launcher will use PATH uv for local builds");
    }

    let mtls_identity_target = out_dir.join("launcher_mtls_identity.pem");
    let mtls_identity = match env::var(MTLS_IDENTITY_ENV) {
        Ok(encoded) if !encoded.trim().is_empty() => Some(
            BASE64_STANDARD
                .decode(encoded.trim().as_bytes())
                .expect("decode ALAS_LAUNCHER_MTLS_IDENTITY_PEM_B64"),
        ),
        _ => {
            let manifest_dir =
                PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
            let local_cert_path = manifest_dir.join("证书.txt");
            if local_cert_path.exists() {
                println!(
                    "cargo:warning=ALAS_LAUNCHER_MTLS_IDENTITY_PEM_B64 is not set; using local {}",
                    local_cert_path.display()
                );
                println!("cargo:rerun-if-changed={}", local_cert_path.display());
                Some(fs::read(&local_cert_path).expect("read local certificate file"))
            } else {
                if env::var_os(REQUIRE_MTLS_ENV).is_some() {
                    panic!("ALAS_LAUNCHER_MTLS_IDENTITY_PEM_B64 is required but not set");
                }
                println!(
                    "cargo:warning=ALAS_LAUNCHER_MTLS_IDENTITY_PEM_B64 is not set; launcher update client will build without mTLS identity"
                );
                None
            }
        }
    };
    match mtls_identity {
        Some(bytes) => {
            fs::write(&mtls_identity_target, bytes).expect("write launcher mTLS identity")
        }
        None => {
            fs::write(&mtls_identity_target, []).expect("write empty launcher mTLS placeholder")
        }
    }
}

/// 版本一致性守卫。
///
/// 版本号在仓库里还有两个副本：`tauri.conf.json` 的 `version`、
/// `Info.plist` 的 `CFBundleShortVersionString`。CI 的 `Prepare version from tag`
/// 步骤会按 tag 一并改写它们，但**只在 CI 工作区内改写、从不提交回仓库**，
/// 所以这几个字面量很容易一直停在旧值上（本地 `cargo build` 出来的包就会带着旧版本元数据）。
///
/// 这里在构建时比对一次，不一致就打印警告。**只警告，不中断构建。**
fn check_version_consistency() {
    let expected = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    if expected.is_empty() {
        return;
    }

    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=Info.plist");

    let mut stale = Vec::new();

    if let Some(actual) = read_tauri_conf_version() {
        if actual != expected {
            stale.push(format!(r#"tauri.conf.json 的 "version" 是 {actual}"#));
        }
    }

    if let Some(actual) = read_plist_short_version() {
        if actual != expected {
            stale.push(format!(
                "Info.plist 的 CFBundleShortVersionString 是 {actual}"
            ));
        }
    }

    if stale.is_empty() {
        return;
    }

    println!(
        "cargo:warning=Cargo.toml 的版本是 {expected}，但 {}。\
         发版时 CI 会按 tag 把这些值改对，因此只影响本地构建；\
         建议把仓库里的这些字面量一并同步为 {expected}。",
        stale.join("；")
    );
}

/// 从 `tauri.conf.json` 取顶层 `"version"` 的值。
///
/// 刻意不做 JSON 解析：`[build-dependencies]` 里没有 JSON 解析器，
/// 为一个只用来打印警告的检查引入新依赖并不值得。
/// 顶层字段缩进固定为 2 个空格，子对象里的同名字段缩进更深，据此过滤。
fn read_tauri_conf_version() -> Option<String> {
    let text = fs::read_to_string("tauri.conf.json").ok()?;
    for line in text.lines() {
        if line.len() - line.trim_start().len() != 2 {
            continue;
        }
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("\"version\"") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_owned());
        }
    }
    None
}

/// 从 `Info.plist` 取 `CFBundleShortVersionString` 的值。
fn read_plist_short_version() -> Option<String> {
    let text = fs::read_to_string("Info.plist").ok()?;
    let key_at = text.find("<key>CFBundleShortVersionString</key>")?;
    let after_key = &text[key_at..];
    let open_at = after_key.find("<string>")?;
    let after_open = &after_key[open_at + "<string>".len()..];
    let close_at = after_open.find("</string>")?;
    Some(after_open[..close_at].trim().to_owned())
}
