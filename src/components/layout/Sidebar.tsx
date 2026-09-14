import { Globe, PanelLeftClose, PanelLeftOpen, ShieldAlert } from "lucide-react";
import { NAV_ITEMS } from "@/lib/navigation";
import { useTranslation } from "@/hooks/useTranslation";
import { cn, formatBytes } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";

export function Sidebar() {
  const activeModule = useAppStore((state) => state.activeModule);
  const setActiveModule = useAppStore((state) => state.setActiveModule);
  const collapsed = useAppStore((state) => state.sidebarCollapsed);
  const toggleSidebar = useAppStore((state) => state.toggleSidebar);
  const freedThisSession = useAppStore((state) => state.freedThisSession);
  const fullDiskAccess = useAppStore((state) => state.permissions.fullDiskAccess);
  const { t, lang, toggleLang } = useTranslation();

  return (
    <aside
      className={cn(
        "flex h-full shrink-0 flex-col border-r border-sidebar-border bg-sidebar/70 backdrop-blur-2xl backdrop-saturate-150 transition-[width] duration-200 ease-out",
        collapsed ? "w-[68px]" : "w-[228px]",
      )}
    >
      {/* Traffic lights live here, so the whole strip is a drag handle. */}
      <div className="drag-region flex h-[52px] items-center justify-between px-4 pt-1">
        {!collapsed ? (
          <div className="flex items-center gap-2 pl-[68px]">
            <span className="text-[15px] font-semibold tracking-tight">
              Windle
            </span>
          </div>
        ) : null}
        <button
          type="button"
          onClick={toggleLang}
          title={lang === "zh-CN" ? "切换为 English" : "Switch to 中文"}
          className="no-drag flex items-center gap-1.5 rounded-lg px-2 py-1 text-[11px] font-semibold text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground"
        >
          <Globe className="size-3.5" />
          {lang === "zh-CN" ? "中" : "EN"}
        </button>
      </div>

      <nav className="flex-1 overflow-y-auto px-2.5 py-2">
        <ul className="flex flex-col gap-0.5">
          {NAV_ITEMS.map(({ id, labelKey, descKey, icon: Icon }) => {
            const isActive = id === activeModule;
            const label = t(labelKey);
            const desc = t(descKey);

            return (
              <li key={id}>
                <button
                  type="button"
                  onClick={() => setActiveModule(id)}
                  title={collapsed ? label : undefined}
                  aria-current={isActive ? "page" : undefined}
                  className={cn(
                    "no-drag group flex w-full items-center gap-2.5 rounded-lg px-2.5 py-[7px] text-left text-[13px] font-medium transition-colors duration-100 outline-none focus-visible:ring-2 focus-visible:ring-ring/60",
                    collapsed && "justify-center px-0",
                    isActive
                      ? "bg-primary text-primary-foreground shadow-sm"
                      : "text-sidebar-foreground/80 hover:bg-sidebar-accent hover:text-sidebar-foreground",
                  )}
                >
                  <Icon
                    className={cn(
                      "size-[17px] shrink-0",
                      isActive
                        ? "text-primary-foreground"
                        : "text-muted-foreground group-hover:text-sidebar-foreground",
                    )}
                    strokeWidth={isActive ? 2.2 : 1.9}
                  />
                  {!collapsed && (
                    <span className="truncate" title={desc}>
                      {label}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </nav>

      <div className="flex flex-col gap-2 border-t border-sidebar-border px-2.5 py-2.5">
        {!fullDiskAccess && !collapsed && (
          <div className="flex items-start gap-2 rounded-lg bg-warning/12 px-2.5 py-2 text-[11px] leading-snug text-muted-foreground">
            <ShieldAlert className="mt-px size-3.5 shrink-0 text-warning" />
            <span>
              {t("permission.fullDiskAccessOff")}
            </span>
          </div>
        )}

        {!collapsed && freedThisSession > 0 && (
          <p className="px-2.5 text-[11px] text-muted-foreground">
            {t("sidebar.freed")}{" "}
            <span className="font-semibold text-success">
              {formatBytes(freedThisSession)}
            </span>{" "}
            {t("sidebar.thisSession")}
          </p>
        )}

        <button
          type="button"
          onClick={toggleSidebar}
          title={collapsed ? t("sidebar.expand") : t("sidebar.collapse")}
          className={cn(
            "no-drag flex items-center gap-2.5 rounded-lg px-2.5 py-[7px] text-[13px] font-medium text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground",
            collapsed && "justify-center px-0",
          )}
        >
          {collapsed ? (
            <PanelLeftOpen className="size-[17px]" strokeWidth={1.9} />
          ) : (
            <>
              <PanelLeftClose className="size-[17px]" strokeWidth={1.9} />
              <span>{t("sidebar.collapse")}</span>
            </>
          )}
        </button>
      </div>
    </aside>
  );
}
