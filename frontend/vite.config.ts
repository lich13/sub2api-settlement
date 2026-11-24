import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import pkg from "./package.json";

export default defineConfig({
  base: "/settlement/",
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  plugins: [react()],
  server: {
    proxy: {
      "/settlement/api": "http://127.0.0.1:18082",
      "/settlement/sso": "http://127.0.0.1:18082",
    },
  },
});
