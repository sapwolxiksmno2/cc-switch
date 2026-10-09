import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";

export type UpdateChannel = "stable" | "beta";

export interface UpdateInfo {
  currentVersion: string;
  availableVersion: string;
  notes?: string;
  pubDate?: string;
}

export interface CheckOptions {
  timeout?: number;
  channel?: UpdateChannel;
}

export async function getCurrentVersion(): Promise<string> {
  try {
    return await getVersion();
  } catch {
    return "";
  }
}

export async function checkForUpdate(
  opts: CheckOptions = {},
): Promise<
  { status: "up-to-date" } | { status: "available"; info: UpdateInfo }
> {
  // Portable 仅通过后端查询版本元数据，不导入 updater 插件。
  if (await invoke<boolean>("is_portable_mode")) {
    const availableVersion = await invoke<string | null>(
      "check_app_update_available",
    );
    if (!availableVersion) return { status: "up-to-date" };
    return {
      status: "available",
      info: { currentVersion: await getCurrentVersion(), availableVersion },
    };
  }

  const currentVersion = await getCurrentVersion();
  // 动态引入，避免在未安装插件时导致打包期问题
  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check({ timeout: opts.timeout ?? 30000 } as any);

  if (!update) {
    return { status: "up-to-date" };
  }

  const info: UpdateInfo = {
    currentVersion,
    availableVersion: (update as any).version ?? "",
    notes: (update as any).notes,
    pubDate: (update as any).date,
  };

  return { status: "available", info };
}
