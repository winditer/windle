import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import App from "./App";
import { MenuBarApp } from "@/components/menubar/MenuBarApp";
import { LanguageProvider } from "@/i18n";
import "./styles/globals.css";

let isMenuBar = false;
try {
  isMenuBar = getCurrentWebviewWindow().label === "menubar";
} catch {
  // Fallback to main app if Tauri not yet injected
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <LanguageProvider>
      {isMenuBar ? <MenuBarApp /> : <App />}
    </LanguageProvider>
  </React.StrictMode>,
);
