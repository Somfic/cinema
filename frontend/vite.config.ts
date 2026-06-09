import { sveltekit } from '@sveltejs/kit/vite';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { defineConfig } from 'vite';

// Fail fast if the schema codegen output is missing. Without it every
// `import { api } from "$lib/schema"` resolves to nothing and vite emits a
// cascade of unrelated "module not found" errors. Catching it here points
// the user at the actual fix instead of letting them dig.
const here = dirname(fileURLToPath(import.meta.url));
const schemaIndex = resolve(here, 'src/lib/schema/index.ts');
if (!existsSync(schemaIndex)) {
	throw new Error(
		[
			'',
			'  cinema: frontend/src/lib/schema/index.ts is missing.',
			'',
			'  The schema is generated from the Rust traits in `src/api/`.',
			'  Run one of these from the repo root, then retry:',
			'',
			'    just schema     # if you have `just` installed',
			'',
			'  or manually:',
			'',
			'    cargo run -p cinema-schema-codegen -- --rust-only',
			'    TS_RS_EXPORT_DIR="$PWD/target/schema-bindings" cargo test export_bindings',
			'    TS_RS_EXPORT_DIR="$PWD/target/schema-bindings" cargo run -p cinema-schema-codegen',
			'',
		].join('\n'),
	);
}

export default defineConfig({
	plugins: [sveltekit()],
	// `glow` ships uncompiled `.svelte` files (it sets the `svelte` export
	// condition). Exclude it from esbuild dep-optimization so the svelte plugin
	// compiles it via the normal pipeline — otherwise a forced re-optimization
	// hands glow's `.svelte` files to esbuild, which has no loader for them.
	optimizeDeps: {
		exclude: ['glow'],
	},
	server: {
		port: 5174,
		hmr: {
			clientPort: Number(process.env.VITE_HMR_PORT) || 5174,
		},
		fs: {
			allow: ['..'],
		},
	},
});
