//! Application proxy for the managed ChatGPT/Codex desktop app.
//!
//! Configures Chromium proxy arguments for that app only. Nothing here touches
//! the Windows system proxy, proxy services, or the terminal code CLI.
//! Chromium reads proxy flags only on the first process start, so callers must
//! probe before closing anything and surface a restart-required state instead
//! of silently reusing a running instance.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 10808;
/// Total wall-clock budget for one connection probe.
pub const PROBE_BUDGET: Duration = Duration::from_secs(3);

/// Complete loopback + private-range list from the reference launcher. Chromium
/// matches these patterns literally, so entries are kept verbatim.
const DEFAULT_BYPASS_LIST: [&str; 22] = [
    "<-loopback>",
    "localhost",
    "127.0.0.1",
    "::1",
    "10.*",
    "172.16.*",
    "172.17.*",
    "172.18.*",
    "172.19.*",
    "172.20.*",
    "172.21.*",
    "172.22.*",
    "172.23.*",
    "172.24.*",
    "172.25.*",
    "172.26.*",
    "172.27.*",
    "172.28.*",
    "172.29.*",
    "172.30.*",
    "172.31.*",
    "192.168.*",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppProxyType {
    #[default]
    Http,
    Socks5,
}

impl AppProxyType {
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Socks5 => "socks5",
        }
    }
}

/// Persisted in `updater.json` / the per-user state overlay. Missing fields fall
/// back to the disabled default so older config files keep working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppProxySettings {
    pub enabled: bool,
    #[serde(rename = "type")]
    pub kind: AppProxyType,
    pub host: String,
    pub port: u16,
    pub disable_quic: bool,
    pub bypass_list: Vec<String>,
}

impl Default for AppProxySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: AppProxyType::Http,
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            disable_quic: true,
            bypass_list: DEFAULT_BYPASS_LIST
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
        }
    }
}

impl AppProxySettings {
    /// Trim user-entered values without discarding the order or content of the
    /// bypass list. Empty entries are the only thing dropped.
    pub fn normalized(&self) -> Self {
        let mut settings = self.clone();
        settings.host = settings.host.trim().to_owned();
        settings.bypass_list = settings
            .bypass_list
            .iter()
            .map(|entry| entry.trim().to_owned())
            .filter(|entry| !entry.is_empty())
            .collect();
        settings
    }

    pub fn validate(&self) -> Result<(), String> {
        let settings = self.normalized();
        let host = settings.host.as_str();
        if host.is_empty() {
            return Err("代理地址不能为空".into());
        }
        if host.contains("://") {
            return Err("代理地址只填写主机，不要包含 http:// 或 socks5://".into());
        }
        if host.contains('/') || host.contains('\\') {
            return Err("代理地址只填写主机，不要包含路径".into());
        }
        if host
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == ';')
        {
            return Err("代理地址不能包含空格、引号或分号".into());
        }
        if settings.port == 0 {
            return Err("本地端口必须在 1 到 65535 之间".into());
        }
        if settings.bypass_list.is_empty() {
            return Err("直连列表至少保留一项".into());
        }
        if settings
            .bypass_list
            .iter()
            .any(|entry| entry.contains(';') || entry.contains('"'))
        {
            return Err("直连列表不能包含分号或引号".into());
        }
        Ok(())
    }

    /// Chromium argument vector. Empty when the feature is disabled, so launch
    /// falls back to the existing behavior.
    pub fn chromium_arguments(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let settings = self.normalized();
        let mut arguments = vec![format!("--proxy-server={}", proxy_url(&settings))];
        if !settings.bypass_list.is_empty() {
            arguments.push(format!(
                "--proxy-bypass-list={}",
                settings.bypass_list.join(";")
            ));
        }
        if settings.disable_quic {
            arguments.push("--disable-quic".into());
        }
        arguments
    }
}

/// IPv6 literals need brackets inside a proxy URL; accept them already
/// bracketed so a pasted `[::1]` is not double-wrapped.
fn host_for_url(host: &str) -> String {
    if host.starts_with('[') || !host.contains(':') {
        host.to_owned()
    } else {
        format!("[{host}]")
    }
}

fn resolve_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

pub fn proxy_url(settings: &AppProxySettings) -> String {
    let settings = settings.normalized();
    format!(
        "{}://{}:{}",
        settings.kind.scheme(),
        host_for_url(&settings.host),
        settings.port
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppProxyProbeOutcome {
    Success,
    InvalidConfiguration,
    ResolutionFailed,
    ConnectionRefused,
    Timeout,
    ConnectionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppProxyProbe {
    pub outcome: AppProxyProbeOutcome,
    pub message: String,
}

impl AppProxyProbe {
    fn new(outcome: AppProxyProbeOutcome, message: String) -> Self {
        Self { outcome, message }
    }
}

/// Probe the configured endpoint with one total budget, shared by the manual
/// test and the pre-launch check.
pub fn probe(settings: &AppProxySettings) -> AppProxyProbe {
    let settings = settings.normalized();
    if let Err(message) = settings.validate() {
        return AppProxyProbe::new(AppProxyProbeOutcome::InvalidConfiguration, message);
    }

    let deadline = Instant::now() + PROBE_BUDGET;
    let addresses = match resolve(&settings.host, settings.port, PROBE_BUDGET) {
        Ok(addresses) => addresses,
        Err(outcome) => return outcome_message(outcome, &settings),
    };

    let mut last_error = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return outcome_message(AppProxyProbeOutcome::Timeout, &settings);
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(_) => return connected_message(&settings),
            Err(error) => last_error = Some(error),
        }
    }

    match last_error {
        Some(error) => classify_error(&error, &settings),
        None => outcome_message(AppProxyProbeOutcome::ResolutionFailed, &settings),
    }
}

/// DNS lookup can block far longer than the probe budget, so it runs on a
/// helper thread we stop waiting for when the budget expires.
fn resolve(
    host: &str,
    port: u16,
    budget: Duration,
) -> Result<Vec<SocketAddr>, AppProxyProbeOutcome> {
    let (sender, receiver) = mpsc::channel();
    let host = resolve_host(host).to_owned();
    std::thread::spawn(move || {
        let resolved = (host.as_str(), port)
            .to_socket_addrs()
            .map(|iter| iter.collect::<Vec<_>>());
        let _ = sender.send(resolved);
    });
    match receiver.recv_timeout(budget) {
        Ok(Ok(addresses)) if !addresses.is_empty() => Ok(addresses),
        Ok(_) => Err(AppProxyProbeOutcome::ResolutionFailed),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AppProxyProbeOutcome::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(AppProxyProbeOutcome::ResolutionFailed),
    }
}

fn classify_error(error: &std::io::Error, settings: &AppProxySettings) -> AppProxyProbe {
    let outcome = match error.kind() {
        ErrorKind::ConnectionRefused => AppProxyProbeOutcome::ConnectionRefused,
        ErrorKind::TimedOut | ErrorKind::WouldBlock => AppProxyProbeOutcome::Timeout,
        _ => AppProxyProbeOutcome::ConnectionFailed,
    };
    outcome_message(outcome, settings)
}

fn outcome_message(outcome: AppProxyProbeOutcome, settings: &AppProxySettings) -> AppProxyProbe {
    let endpoint = format!("{}:{}", settings.host, settings.port);
    let message = match outcome {
        AppProxyProbeOutcome::Success => format!("已连接到 {endpoint}"),
        AppProxyProbeOutcome::InvalidConfiguration => "代理配置无效".to_owned(),
        AppProxyProbeOutcome::ResolutionFailed => format!(
            "无法解析代理地址 {}，请确认代理软件正在运行并检查地址是否填写正确。",
            settings.host
        ),
        AppProxyProbeOutcome::ConnectionRefused => format!(
            "代理地址 {endpoint} 拒绝连接，请确认代理软件已启动，并填写它显示的本地 HTTP / mixed 或 SOCKS5 端口。"
        ),
        AppProxyProbeOutcome::Timeout => {
            format!("连接 {endpoint} 超时，请确认代理软件正在运行且端口与界面显示一致。")
        }
        AppProxyProbeOutcome::ConnectionFailed => format!("无法连接 {endpoint}，请检查代理软件和端口设置。"),
    };
    AppProxyProbe::new(outcome, message)
}

fn connected_message(settings: &AppProxySettings) -> AppProxyProbe {
    AppProxyProbe::new(
        AppProxyProbeOutcome::Success,
        format!(
            "已连接到 {}:{}。这表示中文助手可以连接到这个本地端口，不代表远程节点或 ChatGPT 已经可用。",
            settings.host, settings.port
        ),
    )
}

/// Fail the launch before any managed process is closed.
pub fn ensure_reachable(settings: &AppProxySettings) -> Result<()> {
    if !settings.enabled {
        return Ok(());
    }
    let probe = probe(settings);
    if probe.outcome == AppProxyProbeOutcome::Success {
        return Ok(());
    }
    bail!("{}", probe.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn local_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture listener");
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    fn settings_with(port: u16) -> AppProxySettings {
        AppProxySettings {
            enabled: true,
            port,
            ..AppProxySettings::default()
        }
    }

    #[test]
    fn defaults_match_the_reference_launcher() {
        let settings = AppProxySettings::default();
        assert!(!settings.enabled);
        assert_eq!(settings.kind, AppProxyType::Http);
        assert_eq!(settings.host, "127.0.0.1");
        assert_eq!(settings.port, 10808);
        assert!(settings.disable_quic);
        assert_eq!(settings.bypass_list.len(), 22);
        assert_eq!(settings.bypass_list[0], "<-loopback>");
        assert_eq!(settings.bypass_list[21], "192.168.*");
    }

    #[test]
    fn old_config_without_proxy_deserializes_to_disabled_default() {
        let cfg: AppProxySettings = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, AppProxySettings::default());
    }

    #[test]
    fn validation_rejects_urls_paths_and_empty_values() {
        let base = AppProxySettings::default();
        for host in [
            " ",
            "http://127.0.0.1",
            "socks5://127.0.0.1",
            "127.0.0.1/proxy",
        ] {
            let settings = AppProxySettings {
                host: host.into(),
                ..base.clone()
            };
            assert!(settings.validate().is_err(), "accepted host {host:?}");
        }
        let portless = AppProxySettings {
            port: 0,
            ..base.clone()
        };
        assert!(portless.validate().is_err());
        let no_bypass = AppProxySettings {
            bypass_list: vec!["  ".into()],
            ..base
        };
        assert!(no_bypass.validate().is_err());
    }

    #[test]
    fn validation_accepts_ipv6_and_private_hosts() {
        for host in ["::1", "[::1]", "127.0.0.1", "localhost", "10.0.0.5"] {
            let settings = AppProxySettings {
                host: host.into(),
                ..AppProxySettings::default()
            };
            assert!(settings.validate().is_ok(), "rejected host {host:?}");
        }
    }

    #[test]
    fn proxy_url_brackets_ipv6_hosts_once() {
        let ipv6 = AppProxySettings {
            host: "::1".into(),
            ..AppProxySettings::default()
        };
        assert_eq!(proxy_url(&ipv6), "http://[::1]:10808");
        let bracketed = AppProxySettings {
            host: "[::1]".into(),
            kind: AppProxyType::Socks5,
            port: 1080,
            ..AppProxySettings::default()
        };
        assert_eq!(proxy_url(&bracketed), "socks5://[::1]:1080");
    }

    #[test]
    fn chromium_arguments_are_an_argument_vector() {
        assert!(AppProxySettings::default().chromium_arguments().is_empty());

        let enabled = settings_with(10808);
        assert_eq!(
            enabled.chromium_arguments(),
            vec![
                "--proxy-server=http://127.0.0.1:10808".to_owned(),
                format!("--proxy-bypass-list={}", DEFAULT_BYPASS_LIST.join(";")),
                "--disable-quic".to_owned(),
            ]
        );

        let socks = AppProxySettings {
            kind: AppProxyType::Socks5,
            port: 1080,
            disable_quic: false,
            bypass_list: vec![" localhost ".into(), "10.*".into()],
            ..settings_with(1080)
        };
        assert_eq!(
            socks.chromium_arguments(),
            vec![
                "--proxy-server=socks5://127.0.0.1:1080".to_owned(),
                "--proxy-bypass-list=localhost;10.*".to_owned(),
            ]
        );
    }

    #[test]
    fn probe_reports_success_against_a_local_listener() {
        let (_listener, port) = local_listener();
        let probe = probe(&settings_with(port));
        assert_eq!(probe.outcome, AppProxyProbeOutcome::Success);
        assert!(probe.message.contains("已连接到"));
    }

    #[test]
    fn probe_reports_refused_when_nothing_listens() {
        let (listener, port) = local_listener();
        drop(listener);
        let probe = probe(&settings_with(port));
        assert_eq!(probe.outcome, AppProxyProbeOutcome::ConnectionRefused);
        assert!(probe.message.contains("拒绝连接"));
    }

    /// `.invalid` is reserved by RFC 2606, so this host never resolves. A slow
    /// resolver exhausts the budget first and reports `Timeout`; either way the
    /// outcome stays structured and Chinese-facing instead of succeeding.
    #[test]
    fn probe_never_reports_success_for_an_unresolvable_host() {
        let unresolvable = AppProxySettings {
            enabled: true,
            host: "proxy.codex-launcher.invalid".into(),
            ..AppProxySettings::default()
        };
        let probe = probe(&unresolvable);
        assert!(
            matches!(
                probe.outcome,
                AppProxyProbeOutcome::ResolutionFailed | AppProxyProbeOutcome::Timeout
            ),
            "unexpected outcome {:?}",
            probe.outcome
        );
        assert!(
            probe.message.contains("无法解析") || probe.message.contains("超时"),
            "{}",
            probe.message
        );
    }

    #[test]
    fn resolution_failure_has_a_chinese_message() {
        let message = outcome_message(
            AppProxyProbeOutcome::ResolutionFailed,
            &settings_with(10808),
        )
        .message;
        assert!(message.contains("无法解析"), "{message}");
    }

    #[test]
    fn probe_rejects_invalid_configuration_without_connecting() {
        let invalid = AppProxySettings {
            enabled: true,
            host: "http://127.0.0.1".into(),
            ..AppProxySettings::default()
        };
        assert_eq!(
            probe(&invalid).outcome,
            AppProxyProbeOutcome::InvalidConfiguration
        );
    }

    #[test]
    fn io_errors_map_to_structured_outcomes() {
        let settings = AppProxySettings::default();
        let refused = std::io::Error::from(ErrorKind::ConnectionRefused);
        assert_eq!(
            classify_error(&refused, &settings).outcome,
            AppProxyProbeOutcome::ConnectionRefused
        );
        let timed_out = std::io::Error::from(ErrorKind::TimedOut);
        assert_eq!(
            classify_error(&timed_out, &settings).outcome,
            AppProxyProbeOutcome::Timeout
        );
        let other = std::io::Error::from(ErrorKind::AddrNotAvailable);
        assert_eq!(
            classify_error(&other, &settings).outcome,
            AppProxyProbeOutcome::ConnectionFailed
        );
    }

    #[test]
    fn disabled_settings_never_block_launch() {
        let mut settings = settings_with(1);
        settings.enabled = false;
        assert!(ensure_reachable(&settings).is_ok());
    }
}
