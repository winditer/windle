import {
  Activity,
  Bot,
  Download,
  FolderCode,
  HardDrive,
  LayoutDashboard,
  Package,
  Ship,
  Trash2,
  Zap,
  type LucideIcon,
} from "lucide-react";
import type { ModuleId } from "@/types";
import type { TranslationKey } from "@/i18n/translations";

export interface NavItem {
  id: ModuleId;
  labelKey: TranslationKey;
  descKey: TranslationKey;
  icon: LucideIcon;
}

export const NAV_ITEMS: NavItem[] = [
  {
    id: "dashboard",
    labelKey: "nav.dashboard",
    descKey: "nav.dashboard.desc",
    icon: LayoutDashboard,
  },
  {
    id: "clean",
    labelKey: "nav.deepClean",
    descKey: "nav.deepClean.desc",
    icon: Trash2,
  },
  {
    id: "uninstall",
    labelKey: "nav.smartUninstall",
    descKey: "nav.smartUninstall.desc",
    icon: Package,
  },
  {
    id: "analyze",
    labelKey: "nav.diskAnalyzer",
    descKey: "nav.diskAnalyzer.desc",
    icon: HardDrive,
  },
  {
    id: "optimize",
    labelKey: "nav.systemOptimize",
    descKey: "nav.systemOptimize.desc",
    icon: Zap,
  },
  {
    id: "monitor",
    labelKey: "nav.liveMonitor",
    descKey: "nav.liveMonitor.desc",
    icon: Activity,
  },
  {
    id: "purge",
    labelKey: "nav.projectPurge",
    descKey: "nav.projectPurge.desc",
    icon: FolderCode,
  },
  {
    id: "installer",
    labelKey: "nav.installerCleanup",
    descKey: "nav.installerCleanup.desc",
    icon: Download,
  },
  {
    id: "docker",
    labelKey: "nav.docker",
    descKey: "nav.docker.desc",
    icon: Ship,
  },
  {
    id: "agent",
    labelKey: "nav.aiAgentCleanup",
    descKey: "nav.aiAgentCleanup.desc",
    icon: Bot,
  },
];

export const NAV_BY_ID = Object.fromEntries(
  NAV_ITEMS.map((item) => [item.id, item]),
) as Record<ModuleId, NavItem>;
