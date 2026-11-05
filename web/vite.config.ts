import { readFile } from "node:fs/promises";
import type { IncomingMessage, ServerResponse } from "node:http";
import { extname, resolve, sep } from "node:path";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig, type Plugin, type ProxyOptions } from "vite";

const BACKEND = "http://127.0.0.1:8787";
const OWNER = "dev";
const RENDER_DIR = resolve(import.meta.dirname, "../render/dist");

function backend(ws = false): ProxyOptions {
	return {
		target: BACKEND,
		ws,
		headers: { "X-Agent-Reports-Owner": OWNER },
		configure(proxy) {
			proxy.on("proxyReqWs", proxyReq => {
				proxyReq.setHeader("X-Agent-Reports-Owner", OWNER);
			});
		},
	};
}

/** Only `/api` reaches the Rust server; everything else is static (Vite or nginx). */
const proxy: Record<string, ProxyOptions> = {
	"/api": backend(true),
};

/** Dev/preview: serve the share shell for `/s/{token}` like nginx does in production. */
function shareShell(): Plugin {
	const rewrite = (req: { url?: string }, _res: unknown, next: () => void) => {
		if (req.url && /^\/s\/[^/?]+\/?(\?.*)?$/.test(req.url)) req.url = "/share.html";
		next();
	};
	return {
		name: "agent-reports-share-shell",
		configureServer(server) {
			server.middlewares.use(rewrite);
		},
		configurePreviewServer(server) {
			server.middlewares.use(rewrite);
		},
	};
}

const MIME: Record<string, string> = {
	".css": "text/css; charset=utf-8",
	".gif": "image/gif",
	".html": "text/html; charset=utf-8",
	".jpeg": "image/jpeg",
	".jpg": "image/jpeg",
	".js": "text/javascript; charset=utf-8",
	".json": "application/json; charset=utf-8",
	".map": "application/json; charset=utf-8",
	".mjs": "text/javascript; charset=utf-8",
	".png": "image/png",
	".svg": "image/svg+xml",
	".ttf": "font/ttf",
	".wasm": "application/wasm",
	".webp": "image/webp",
	".woff": "font/woff",
	".woff2": "font/woff2",
};

/**
 * Dev/preview: serve `render/dist` at `/_render/`, which nginx serves in production.
 * The runtime is loaded as a module script from a sandboxed (opaque origin) iframe,
 * so every response needs `Access-Control-Allow-Origin`.
 */
function renderStatic(): Plugin {
	const handle = (req: IncomingMessage, res: ServerResponse, next: () => void): void => {
		const url = req.url ?? "";
		if (!url.startsWith("/_render/")) {
			next();
			return;
		}
		let name: string;
		try {
			name = decodeURIComponent(url.slice("/_render/".length).split("?")[0]);
		} catch {
			res.writeHead(400, corsHeaders(0)).end();
			return;
		}
		const path = resolve(RENDER_DIR, name);
		if (!path.startsWith(RENDER_DIR + sep)) {
			res.writeHead(404, corsHeaders(0)).end();
			return;
		}
		readFile(path)
			.then(body => {
				res.writeHead(200, { ...corsHeaders(body.length), ...cacheHeaders(path) }).end(body);
			})
			.catch(() => {
				res.writeHead(404, corsHeaders(0)).end();
			});
	};
	return {
		name: "agent-reports-render-static",
		configureServer(server) {
			server.middlewares.use(handle);
		},
		configurePreviewServer(server) {
			server.middlewares.use(handle);
		},
	};
}

function corsHeaders(length: number): Record<string, string | number> {
	return {
		"Access-Control-Allow-Origin": "*",
		"Content-Length": length,
	};
}

function cacheHeaders(path: string): Record<string, string> {
	const name = path.slice(RENDER_DIR.length + 1);
	const immutable = name !== "runtime.js" && name !== "report.css";
	return {
		"Cache-Control": immutable ? "public, max-age=31536000, immutable" : "no-cache",
		"Content-Type": MIME[extname(path)] ?? "application/octet-stream",
	};
}

export default defineConfig({
	plugins: [shareShell(), renderStatic(), svelte()],
	server: { port: 5173, strictPort: true, proxy },
	preview: { port: 4173, strictPort: true, proxy },
	appType: "spa",
	build: {
		target: "es2023",
		// Vite 8 defaults CSS minification to Lightning CSS, which lowers `light-dark()` into
		// `--lightningcss-light/dark` toggled only by `prefers-color-scheme`. That breaks runtime
		// theme switching through inline `color-scheme`, so target browsers that support it natively.
		cssTarget: ["chrome123", "firefox120", "safari17.5"],
		rollupOptions: {
			input: {
				index: resolve(import.meta.dirname, "index.html"),
				share: resolve(import.meta.dirname, "share.html"),
			},
		},
	},
});
