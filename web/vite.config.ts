import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

// En dev : `OMG_LISTEN=127.0.0.1:8080 cargo run` d'un côté, `npm run dev` de l'autre.
export default defineConfig({
  plugins: [vue()],
  server: { proxy: { '/api': 'http://127.0.0.1:8080' } },
  build: { outDir: 'dist', emptyOutDir: true },
});
