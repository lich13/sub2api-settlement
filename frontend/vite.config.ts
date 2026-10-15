import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
export default defineConfig({
  base: "/settlement/",
  plugins: [react()],
  server: {
    proxy: {
      "/settlement/api": "http://127.0.0.1:18082",
      "/settlement/sso": "http://127.0.0.1:18082",
    },
  },
});
