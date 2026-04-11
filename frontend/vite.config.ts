import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		port: 5174,
		hmr: {
			clientPort: Number(process.env.VITE_HMR_PORT) || 5174,
		},
		fs: {
			allow: ['..'],
		},
	}
});
