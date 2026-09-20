import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import App from "./App";
import { MenuBarApp } from "@/components/menubar/MenuBarApp";
import { LanguageProvider } from "@/i18n";
import { getPlatform } from "@/services/platform";
import { useAppStore } from "@/stores/appStore";
import "./styles/globals.css";

let isMenuBar = false;
try {
  isMenuBar = getCurrentWebviewWindow().label === "menubar";
} catch {
  // Fallback to main app if Tauri not yet injected
}

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
          {isMenuBar ? <MenuBarApp /> : <App />}
        </LanguageProvider>
      </React.StrictMode>,
    );
  });
