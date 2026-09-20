import { useEffect, useState, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "@/hooks/useTranslation";
import { cn } from "@/lib/utils";

/**
 * Caption buttons for the frameless Windows main window, drawn to the same
 * metrics as the system ones: 46 px wide, full header height, flat until
 * hovered and red for close.
 */
export function WindowControls() {
  const { t } = useTranslation();
  const appWindow = getCurrentWindow();
  const [maximized, setMaximized] = useState(false);

  // Track the real state so the middle button shows the right glyph no matter
  // whether the change came from here, a double-click on a drag region or the
  // keyboard.
  useEffect(() => {
    let alive = true;
    const sync = () => {
      void appWindow
        .isMaximized()
        .then((value) => {
          if (alive) setMaximized(value);
        })
        .catch(() => {});
    };

    sync();
    const unlisten = appWindow.onResized(sync);
    return () => {
      alive = false;
      void unlisten.then((off) => off());
    };
  }, [appWindow]);

  return (
    <div className="no-drag flex shrink-0 items-stretch self-stretch">
      <CaptionButton
        label={t("window.minimize")}
        onClick={() => void appWindow.minimize().catch(() => {})}
      >
        <Glyph>
          <path d="M0 5.5h10" />
        </Glyph>
      </CaptionButton>

      <CaptionButton
        label={maximized ? t("window.restore") : t("window.maximize")}
        onClick={() => void appWindow.toggleMaximize().catch(() => {})}
      >
        <Glyph>
          {maximized ? (
            <>
              <path d="M2.5 2.5V0.5h7v7h-2" />
              <path d="M0.5 2.5h7v7h-7z" />
            </>
          ) : (
            <path d="M0.5 0.5h9v9h-9z" />
          )}
        </Glyph>
      </CaptionButton>

      <CaptionButton
        label={t("window.close")}
        danger
        onClick={() => void appWindow.close().catch(() => {})}
      >
        <Glyph>
          <path d="M0.5 0.5l9 9m0-9l-9 9" />
        </Glyph>
      </CaptionButton>
    </div>
  );
}

function CaptionButton({
  label,
  danger,
  onClick,
  children,
}: {
  label: string;
  danger?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className={cn(
        "flex w-[46px] items-center justify-center text-foreground/90 transition-colors duration-100 ease-out",
        danger
          ? "hover:bg-[#c42b1c] hover:text-white active:bg-[#b1271a]"
          : "hover:bg-foreground/10 active:bg-foreground/15",
      )}
    >
      {children}
    </button>
  );
}

/** 10×10 glyph box, 1 px strokes — the Windows caption geometry. */
function Glyph({ children }: { children: ReactNode }) {
  return (
    <svg
      width="10"
      height="10"
      viewBox="0 0 10 10"
      fill="none"
      stroke="currentColor"
      strokeWidth={1}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}
