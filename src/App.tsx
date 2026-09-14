import { useEffect, type ComponentType } from "react";
import { Layout } from "@/components/layout/Layout";
import {
  AIAgentCleanup,
  Dashboard,
  DeepClean,
  DiskAnalyzer,
  DockerCleanup,
  InstallerCleanup,
  LiveMonitor,
  ProjectPurge,
  SmartUninstall,
  SystemOptimize,
} from "@/components/features";
import { checkPermissions, getDashboardSummary } from "@/services/dashboard";
import { useAppStore } from "@/stores/appStore";
import type { ModuleId } from "@/types";

/** Every sidebar entry maps to exactly one feature view. */
const MODULE_VIEWS: Record<ModuleId, ComponentType> = {
  dashboard: Dashboard,
  clean: DeepClean,
  uninstall: SmartUninstall,
  analyze: DiskAnalyzer,
  optimize: SystemOptimize,
  monitor: LiveMonitor,
  purge: ProjectPurge,
  installer: InstallerCleanup,
  agent: AIAgentCleanup,
  docker: DockerCleanup,
};

export default function App() {
  const activeModule = useAppStore((state) => state.activeModule);
  const setSummary = useAppStore((state) => state.setSummary);
  const setPermissions = useAppStore((state) => state.setPermissions);

  // Pull the initial system snapshot and permission state once at startup.
  useEffect(() => {
    let cancelled = false;

    async function bootstrap() {
      try {
        const [summary, permissions] = await Promise.all([
          getDashboardSummary(),
          checkPermissions(),
        ]);

        if (cancelled) return;
        setSummary(summary);
        setPermissions(permissions);
      } catch {
        // Backend not available — summary stays null so the Dashboard
        // can surface its own error / retry state.
        if (cancelled) return;
        setPermissions({ fullDiskAccess: false, adminAuthorized: false });
      }
    }

    void bootstrap();
    return () => {
      cancelled = true;
    };
  }, [setSummary, setPermissions]);

  const View = MODULE_VIEWS[activeModule];

  return (
    <Layout>
      {/* Remounting on module change replays the fade, giving page transitions. */}
      <div key={activeModule} className="animate-fade-in">
        <View />
      </div>
    </Layout>
  );
}
