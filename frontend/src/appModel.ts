import type {
  AppProxySettings,
  Fetcher,
  InstallEvent,
  InstallModeDefaults,
  InstallerDefaults,
  LauncherUpdateAction,
  UpdatePolicy,
  UpdateAction,
  UpdateStatus
} from "./bridge";
import type { InstallForm, InstallerStep, WorkspacePanel } from "./appTypes";

/// The wizard deploys the assistant only; the desktop app is installed later
/// from the workspace, so nothing here waits on a download.
export function initialInstallEvent(_fetcher: Fetcher): InstallEvent {
  return {
    kind: "phase",
    title: "正在部署中文助手",
    detail: "正在写入启动器和配置。",
    progress: null,
    version: null,
    message: null
  };
}

export function mergeInstallEvent(current: InstallEvent | null, event: InstallEvent): InstallEvent {
  if (event.kind !== "progress" || event.detail.trim() !== "" || !current) {
    return event;
  }

  return {
    ...event,
    title: current.title,
    detail: current.detail
  };
}

export function formFromMode(defaults: InstallerDefaults, mode: InstallForm["mode"]): InstallForm {
  const modeDefaults =
    defaults.modes.find((candidate) => candidate.mode === mode) ?? defaults.modes[0];

  return {
    mode: modeDefaults.mode,
    root: modeDefaults.defaultRoot,
    createShortcut: modeDefaults.createShortcut,
    createDesktopShortcut: modeDefaults.createDesktopShortcut,
    createAssistantDesktopShortcut: modeDefaults.createAssistantDesktopShortcut,
    registerUninstall: modeDefaults.registerUninstall,
    keepVersions: modeDefaults.keepVersions,
    keepAllVersions: modeDefaults.keepAllVersions,
    fetcher: defaults.recommendedFetcher,
    useCurrentJunction: modeDefaults.useCurrentJunction
  };
}

export function modeSubtitle(mode: InstallModeDefaults) {
  if (mode.mode === "portable") {
    return "自包含目录，不写系统卸载入口";
  }
  if (mode.mode === "system") {
    return "安装到 Program Files，需要管理员权限";
  }
  return "安装到当前用户目录，无需管理员权限";
}

export function toPercent(progress: number | null | undefined) {
  if (progress === null || progress === undefined) {
    return null;
  }
  return Math.round(progress * 100);
}

export const installerStepLabel: Record<InstallerStep, string> = {
  welcome: "欢迎",
  mode: "步骤 1 / 3",
  path: "步骤 2 / 3",
  options: "步骤 3 / 3",
  progress: "正在处理",
  done: "完成",
  error: "错误"
};

export const workspacePanelLabel: Record<WorkspacePanel, string> = {
  home: "概览",
  versions: "版本管理",
  settings: "设置",
  uninstall: "卸载助手",
  launcherUpdate: "启动器更新"
};

export function updateStatusLabel(status: UpdateStatus) {
  if (status.kind === "available") {
    return "有可用更新";
  }
  if (status.kind === "upToDate") {
    return "已是最新";
  }
  if (status.kind === "error") {
    return "更新检查失败";
  }
  return "代理模式";
}

export const fetcherLabels: Record<Fetcher, string> = {
  direct: "直连 Microsoft Store",
  winget: "winget",
  localFile: "本地 MSIX"
};

export const updatePolicyLabels: Record<UpdatePolicy, string> = {
  always: "每次启动",
  daily: "每天",
  weekly: "每周",
  never: "从不"
};

export const updateActionLabels: Record<Exclude<UpdateAction, "updateNow">, string> = {
  notNow: "稍后提醒",
  skipThisVersion: "跳过此版本",
  snoozeOneDay: "1 天后提醒",
  snoozeSevenDays: "7 天后提醒",
  never: "关闭提醒"
};

export const launcherUpdateActionLabels: Record<
  Exclude<LauncherUpdateAction, "updateNow" | "viewRelease">,
  string
> = {
  notNow: "稍后提醒",
  skipThisVersion: "跳过此版本",
  snoozeOneDay: "1 天后提醒",
  snoozeSevenDays: "7 天后提醒",
  never: "关闭提醒"
};

export type AppProxyPreset = {
  id: string;
  label: string;
  type: AppProxySettings["type"];
  host: string;
  port: number;
};

/// Local listener ports only — never node servers, subscriptions, or remote 443.
export const appProxyPresets: AppProxyPreset[] = [
  { id: "v2rayn", label: "V2RayN", type: "http", host: "127.0.0.1", port: 10808 },
  { id: "clash-verge", label: "Clash Verge Rev", type: "http", host: "127.0.0.1", port: 7897 },
  { id: "clash", label: "Clash / Mihomo", type: "http", host: "127.0.0.1", port: 7890 },
  { id: "socks5", label: "通用 SOCKS5", type: "socks5", host: "127.0.0.1", port: 1080 }
];

/// The editor accepts one entry per line; Chromium itself joins with `;`.
export function parseAppProxyBypass(text: string) {
  return text
    .split(/[\n;]/)
    .map((entry) => entry.trim())
    .filter(Boolean);
}

export function formatAppProxyBypass(bypassList: string[]) {
  return bypassList.join("\n");
}

export function appProxySettingsEqual(
  left: AppProxySettings,
  right: AppProxySettings
) {
  return (
    left.enabled === right.enabled &&
    left.type === right.type &&
    left.host === right.host &&
    left.port === right.port &&
    left.disableQuic === right.disableQuic &&
    left.bypassList.length === right.bypassList.length &&
    left.bypassList.every((entry, index) => entry === right.bypassList[index])
  );
}

export function matchAppProxyPreset(settings: AppProxySettings) {
  return (
    appProxyPresets.find(
      (preset) =>
        preset.type === settings.type &&
        preset.host === settings.host &&
        preset.port === settings.port
    ) ?? null
  );
}
