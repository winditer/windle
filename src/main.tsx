import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import App from "./App";
import { MenuBarApp } from "@/components/menubar/MenuBarApp";
import { FloatingWidget } from "@/components/floating/FloatingWidget";
import { LanguageProvider } from "@/i18n";
import { getPlatform } from "@/services/platform";
import { useAppStore } from "@/stores/appStore";
import "./styles/globals.css";

/** Which window this bundle is rendering into; `main` unless told otherwise. */
let label = "main";
try {
  label = getCurrentWebviewWindow().label;
} catch {
  // Fallback to the main app if Tauri is not injected yet.
}

// Transparent windows paint their own shapes, so the page behind them has to
// stay clear (see `globals.css`).
document.documentElement.dataset.window = label;

// The interface swaps window chrome and wording per platform, so the platform
// is resolved before the first paint; `getPlatform` always settles (falling
// back to macOS) and the round-trip is a local call.
void getPlatform()
  .catch(() => "macos" as const)
  .then((platform) => {
    useAppStore.getState().setPlatform(platform);
    ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
      <React.StrictMode>
        <LanguageProvider>
          {label === "menubar" ? (
            <MenuBarApp />
          ) : label === "floating" ? (
            <FloatingWidget />
          ) : (
            <App />
          )}
        </LanguageProvider>
      </React.StrictMode>,
    );
  });
