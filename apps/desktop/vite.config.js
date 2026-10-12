import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";
// Tauri 2 expects a fixed dev port and no obfuscation of the dist layout.
export default defineConfig({
    plugins: [react()],
    resolve: {
        alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
    },
    clearScreen: false,
    server: {
        port: 5273,
        strictPort: true,
        host: "127.0.0.1",
    },
    build: {
        target: "es2022",
        sourcemap: true,
        outDir: "dist",
    },
    test: {
        globals: true,
        environment: "jsdom",
        setupFiles: ["./vitest.setup.ts"],
        include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
        restoreMocks: true,
        clearMocks: true,
    },
});
