import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";
import { applyTheme, cachedTheme } from "./lib/theme";

// Before the first paint, so a light-theme launch does not flash dark.
applyTheme(cachedTheme(), false);

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
