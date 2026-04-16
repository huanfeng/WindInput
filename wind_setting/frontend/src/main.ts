import { createApp } from "vue";
import App from "./App.vue";
import "./app.css";

// Auto dark mode: follow system preference
const mq = window.matchMedia("(prefers-color-scheme: dark)");
function applyTheme(e: MediaQueryListEvent | MediaQueryList) {
  document.documentElement.classList.toggle("dark", e.matches);
}
applyTheme(mq);
mq.addEventListener("change", applyTheme);

createApp(App).mount("#app");
