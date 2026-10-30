use codex_windows_cn::app_proxy::{AppProxySettings, AppProxyType};
use codex_windows_cn::bridge::{
    launch_arguments_for, launch_result_from_outcome, persist_version_settings,
    proxy_launch_status, save_app_proxy_settings, version_inventory, AppProxySettingsRequest,
    VersionSettingsRequest,
};
use codex_windows_cn::config::{Config, InstallMode, UpdatePolicy};
use codex_windows_cn::proxy::LaunchOutcome;
use codex_windows_cn::store::Fetcher;
use codex_windows_cn::versions::AppKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

#[test]
fn proxy_launch_status_reports_latest_installed_codex() {
    let root = TestRoot::new("proxy-launch-success");
    root.write_codex_exe("1.0.0");
    root.write_codex_exe("1.2.0");

    let status = proxy_launch_status(root.path(), &test_config(false));

    assert!(status.managed_install);
    assert_eq!(status.current_version.as_deref(), Some("1.2.0"));
    assert_eq!(status.known_latest.as_deref(), Some("1.2.0"));
    assert!(status.can_launch);
    assert_eq!(status.product_name, "Codex");
    assert!(status.running_versions.is_empty());
    assert!(status
        .codex_exe
        .expect("codex exe path")
        .ends_with(r"1.2.0\Codex.exe"));
    assert_eq!(status.message, "可以启动 Codex");
}

#[test]
fn proxy_launch_status_reports_missing_codex_in_chinese() {
    let root = TestRoot::new("proxy-launch-missing");

    let status = proxy_launch_status(root.path(), &test_config(false));

    assert!(status.managed_install);
    assert_eq!(status.current_version.as_deref(), Some("1.0.0"));
    assert_eq!(status.known_latest.as_deref(), Some("1.2.0"));
    assert!(!status.can_launch);
    assert!(status.codex_exe.is_none());
    assert_eq!(status.product_name, "Codex");
    assert_eq!(status.message, "未找到可启动的 Codex 或 ChatGPT");
}

#[test]
fn proxy_launch_status_prefers_chatgpt_for_unified_packages() {
    let root = TestRoot::new("proxy-launch-chatgpt");
    root.write_codex_exe("26.707.3748.0");
    root.write_exe("26.707.3748.0", "ChatGPT.exe");

    let status = proxy_launch_status(root.path(), &test_config(false));

    assert!(status.can_launch);
    assert_eq!(status.product_name, "ChatGPT");
    assert!(status
        .codex_exe
        .expect("launch path")
        .ends_with(r"26.707.3748.0\ChatGPT.exe"));
    assert_eq!(status.message, "可以启动 ChatGPT");
}

#[test]
fn version_inventory_exposes_default_and_deletion_rules() {
    let root = TestRoot::new("version-inventory");
    root.write_codex_exe("1.0.0");
    root.write_exe("2.0.0", "ChatGPT.exe");

    let inventory = version_inventory(root.path(), &test_config(false)).expect("inventory");

    assert_eq!(inventory.product_name, "ChatGPT");
    assert_eq!(inventory.default_version.as_deref(), Some("2.0.0"));
    assert_eq!(inventory.keep_versions, 2);
    assert!(!inventory.keep_all_versions);
    assert_eq!(inventory.update_policy, UpdatePolicy::Daily);
    assert_eq!(inventory.versions.len(), 2);
    assert!(inventory.versions[0].is_default);
    assert!(inventory.versions.iter().all(|item| item.can_delete));
}

#[test]
fn launch_outcome_becomes_a_structured_switch_request() {
    let result = launch_result_from_outcome(LaunchOutcome::SwitchRequired {
        running_versions: vec!["1.0.0".into()],
        target_version: "2.0.0".into(),
    });

    assert!(!result.launched);
    assert!(result.switch_required);
    assert_eq!(result.version.as_deref(), Some("2.0.0"));
    assert_eq!(result.running_versions, vec!["1.0.0"]);
}

#[test]
fn launched_outcome_reports_the_resolved_product() {
    let result = launch_result_from_outcome(LaunchOutcome::Launched {
        version: "26.707.3748.0".into(),
        app_kind: AppKind::ChatGpt,
    });

    assert!(result.launched);
    assert!(!result.switch_required);
    assert_eq!(result.product_name.as_deref(), Some("ChatGPT"));
    assert_eq!(result.message, "已启动 ChatGPT 26.707.3748.0");
}

fn enabled_proxy(port: u16) -> AppProxySettings {
    AppProxySettings {
        enabled: true,
        kind: AppProxyType::Http,
        host: "127.0.0.1".into(),
        port,
        ..AppProxySettings::default()
    }
}

fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe fixture");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// The pre-launch gate runs before any managed process is closed, so an
/// unreachable proxy reports an error instead of attempting a launch.
#[test]
fn an_unreachable_proxy_blocks_the_launch() {
    let error = launch_arguments_for(&enabled_proxy(closed_port()))
        .expect_err("unreachable proxy must block launch");

    assert!(error.starts_with("未能启动应用："), "{error}");
    assert!(error.contains("拒绝连接"), "{error}");
}

/// The gate is wired as the first step of every managed launch, so a failed
/// probe can never reach `proxy::launch_version` and close a running app.
#[test]
fn every_managed_launch_probes_before_launching() {
    let source = include_str!("../src/bridge.rs");
    let body = source
        .split("pub fn launch_managed_app(")
        .nth(1)
        .expect("launch_managed_app should exist");
    let gate = body
        .find("launch_arguments_for(settings)?")
        .expect("launch_managed_app should run the proxy gate");
    let launch = body
        .find("proxy::launch_version(")
        .expect("launch_managed_app should call proxy::launch_version");

    assert!(gate < launch, "the proxy gate must precede any launch");
}

#[test]
fn a_reachable_proxy_supplies_the_chromium_arguments() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe fixture");
    let port = listener.local_addr().unwrap().port();

    let arguments = launch_arguments_for(&enabled_proxy(port)).expect("reachable proxy");

    assert_eq!(
        arguments[0],
        format!("--proxy-server=http://127.0.0.1:{port}")
    );
    assert!(arguments[1].starts_with("--proxy-bypass-list=<-loopback>;localhost"));
    assert_eq!(arguments[2], "--disable-quic");
    drop(listener);
}

#[test]
fn a_reachable_socks5_proxy_supplies_the_socks_scheme() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe fixture");
    let port = listener.local_addr().unwrap().port();
    let settings = AppProxySettings {
        kind: AppProxyType::Socks5,
        ..enabled_proxy(port)
    };

    let arguments = launch_arguments_for(&settings).expect("reachable proxy");

    assert_eq!(
        arguments[0],
        format!("--proxy-server=socks5://127.0.0.1:{port}")
    );
    drop(listener);
}

#[test]
fn a_disabled_proxy_keeps_the_existing_launch_path() {
    let arguments = launch_arguments_for(&AppProxySettings::default()).expect("disabled proxy");

    assert!(arguments.is_empty());
}

#[test]
fn restart_required_is_reported_as_a_structured_state() {
    let result = launch_result_from_outcome(LaunchOutcome::RestartRequired {
        running_versions: vec!["1.2.0".into()],
        target_version: "1.2.0".into(),
    });

    assert!(!result.launched);
    assert!(!result.switch_required);
    assert!(result.restart_required);
    assert_eq!(result.version.as_deref(), Some("1.2.0"));
    assert!(result.message.contains("第一次启动"), "{}", result.message);
}

#[test]
fn launched_outcome_clears_every_restart_flag() {
    let result = launch_result_from_outcome(LaunchOutcome::Launched {
        version: "1.2.0".into(),
        app_kind: AppKind::Codex,
    });

    assert!(result.launched);
    assert!(!result.restart_required);
    assert!(!result.switch_required);
}

/// Another install holding the Chromium singleton would only receive focus, so
/// it must never be reported as a proxied launch.
#[test]
fn an_external_install_holding_the_singleton_is_not_a_launch() {
    let result = launch_result_from_outcome(LaunchOutcome::ExternalInstallRunning {
        holder: PathBuf::from(r"C:\Other\Codex\Codex.exe"),
    });

    assert!(!result.launched);
    assert!(!result.switch_required);
    assert!(!result.restart_required);
    assert!(result.running_versions.is_empty());
    assert!(
        result
            .message
            .contains("另一个安装位置的 ChatGPT/Codex 正在运行"),
        "{}",
        result.message
    );
}

/// The frontend reads camelCase (`disableQuic`, `bypassList`) while the config
/// file keeps the ADR's snake_case, so the saved result must use this shape.
#[test]
fn saved_proxy_result_serializes_as_camel_case() {
    let root = TestRoot::new("proxy-ipc-shape");
    let mut cfg = test_config(false);
    cfg.save_runtime(root.path()).expect("save initial config");

    let result = save_app_proxy_settings(
        root.path(),
        &mut cfg,
        &AppProxySettingsRequest {
            enabled: true,
            kind: AppProxyType::Socks5,
            host: "127.0.0.1".into(),
            port: 1080,
            disable_quic: false,
            bypass_list: vec!["localhost".into()],
        },
    )
    .expect("save proxy settings");

    let json = serde_json::to_value(&result).expect("serialize result");
    let settings = &json["settings"];
    assert_eq!(settings["type"], "socks5");
    assert_eq!(settings["disableQuic"], false);
    assert_eq!(settings["bypassList"][0], "localhost");
    assert!(settings.get("disable_quic").is_none());
    assert!(settings.get("bypass_list").is_none());
}

#[test]
fn version_settings_are_persisted_and_returned_in_inventory() {
    let root = TestRoot::new("retention-persistence");
    root.write_codex_exe("1.0.0");
    let mut cfg = test_config(false);
    cfg.save_runtime(root.path()).expect("save initial config");

    let result = persist_version_settings(
        root.path(),
        &mut cfg,
        VersionSettingsRequest {
            keep_versions: 7,
            keep_all_versions: true,
            update_policy: UpdatePolicy::Always,
        },
    )
    .expect("persist retention settings");

    assert!(result.applied);
    assert_eq!(result.inventory.keep_versions, 7);
    assert!(result.inventory.keep_all_versions);
    assert_eq!(result.inventory.update_policy, UpdatePolicy::Always);
    let saved = Config::load_runtime(root.path()).expect("reload persisted config");
    assert_eq!(saved.keep_versions, 7);
    assert!(saved.keep_all_versions);
    assert_eq!(saved.update_policy, UpdatePolicy::Always);
}

#[test]
fn reenabling_update_checks_clears_never_suppression() {
    let root = TestRoot::new("update-policy-reenable");
    root.write_codex_exe("1.0.0");
    let mut cfg = test_config(false);
    cfg.update_policy = UpdatePolicy::Never;
    cfg.suppress_until_unix = Some(u64::MAX);
    cfg.launcher_suppress_until_unix = Some(u64::MAX);

    persist_version_settings(
        root.path(),
        &mut cfg,
        VersionSettingsRequest {
            keep_versions: 2,
            keep_all_versions: false,
            update_policy: UpdatePolicy::Daily,
        },
    )
    .expect("reenable update checks");

    assert_eq!(cfg.update_policy, UpdatePolicy::Daily);
    assert_eq!(cfg.suppress_until_unix, None);
    assert_eq!(cfg.launcher_suppress_until_unix, None);
}

fn test_config(use_current_junction: bool) -> Config {
    Config {
        install_mode: InstallMode::User,
        current_version: "1.0.0".into(),
        update_policy: UpdatePolicy::default(),
        last_check_unix: None,
        last_launcher_check_unix: None,
        suppress_until_unix: None,
        known_latest: Some("1.2.0".into()),
        skipped_version: None,
        keep_versions: 2,
        keep_all_versions: false,
        fetcher: Fetcher::Direct,
        use_current_junction,
        register_uninstall: true,
        known_latest_launcher: None,
        skipped_launcher_version: None,
        launcher_suppress_until_unix: None,
        app_proxy: Default::default(),
    }
}

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("codex-windows-cn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test root");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write_codex_exe(&self, version: &str) {
        self.write_exe(version, "Codex.exe");
    }

    fn write_exe(&self, version: &str, name: &str) {
        let version_dir = self.path.join("versions").join(version);
        std::fs::create_dir_all(&version_dir).expect("create version dir");
        std::fs::write(version_dir.join(name), b"test exe").expect("write executable");
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
