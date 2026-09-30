import { useMemo } from "react";
import type { AppInfo } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import { effectiveChoice, resolveMapStyle, type ResolvedMapStyle, type ThemeChoice } from "./themes";

/**
 * Whether the OS is in dark mode. The desktop window is forced dark (for its title bar), which
 * also forces the webview's prefers-color-scheme, so there the app reports what the OS said
 * before that; elsewhere the media query is the OS setting.
 */
export function systemPrefersDark(info: AppInfo | null): boolean {
  if (info?.system_theme) return info.system_theme === "dark";
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** The theme choice in effect (the saved one, or the default). */
export function useThemeChoice(): ThemeChoice | null {
  const saved = useAppStore((s) => s.mapTheme);
  const styleUrl = useAppStore((s) => s.settings?.style_url);
  return styleUrl === undefined ? null : effectiveChoice(saved, styleUrl);
}

/** The basemap to show, or null until settings have loaded. */
export function useMapStyle(): ResolvedMapStyle | null {
  const choice = useThemeChoice();
  const styleUrl = useAppStore((s) => s.settings?.style_url);
  const systemTheme = useAppStore((s) => s.info?.system_theme);
  return useMemo(
    () => (choice === null || styleUrl === undefined ? null : resolveMapStyle(choice, styleUrl, systemPrefersDark(useAppStore.getState().info))),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [choice, styleUrl, systemTheme],
  );
}
