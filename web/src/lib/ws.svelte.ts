export type WsStatus = "connecting" | "open" | "closed";

export interface WsEvent {
	type: string;
	report?: {
		project: string;
		slug: string;
		title: string;
		description: string;
		rev: number;
		created_at: string;
		updated_at: string;
		size: number;
	};
	project?: string;
	slug?: string;
	server_time?: string;
}

type Listener = (event: WsEvent) => void;

/**
 * Reconnecting websocket with exponential backoff. One instance per endpoint:
 * the SPA shares `/api/ws`, a share page opens `/api/s/:token/ws`.
 */
export class LiveSocket {
	status = $state<WsStatus>("closed");

	#path: string;
	#ws: WebSocket | null = null;
	#attempt = 0;
	#timer: number | undefined;
	#listeners = new Set<Listener>();
	#reconnects = new Set<() => void>();
	#everOpened = false;
	#closed = false;

	constructor(path: string) {
		this.#path = path;
	}

	connect(): void {
		if (this.#ws || this.#closed) return;
		const scheme = location.protocol === "https:" ? "wss:" : "ws:";
		let socket: WebSocket;
		try {
			socket = new WebSocket(`${scheme}//${location.host}${this.#path}`);
		} catch {
			this.#scheduleReconnect();
			return;
		}
		this.#ws = socket;
		this.status = "connecting";

		socket.addEventListener("open", () => {
			this.#attempt = 0;
			this.status = "open";
			if (this.#everOpened) for (const fn of this.#reconnects) fn();
			this.#everOpened = true;
		});

		socket.addEventListener("message", event => {
			let data: WsEvent;
			try {
				data = JSON.parse(String(event.data)) as WsEvent;
			} catch {
				return;
			}
			for (const fn of this.#listeners) fn(data);
		});

		socket.addEventListener("close", () => {
			this.#ws = null;
			this.status = "closed";
			this.#scheduleReconnect();
		});

		socket.addEventListener("error", () => {
			socket.close();
		});
	}

	subscribe(fn: Listener): () => void {
		this.#listeners.add(fn);
		return () => {
			this.#listeners.delete(fn);
		};
	}

	onReconnect(fn: () => void): () => void {
		this.#reconnects.add(fn);
		return () => {
			this.#reconnects.delete(fn);
		};
	}

	close(): void {
		this.#closed = true;
		if (this.#timer !== undefined) window.clearTimeout(this.#timer);
		this.#timer = undefined;
		this.#ws?.close();
		this.#ws = null;
	}

	#scheduleReconnect(): void {
		if (this.#closed || this.#timer !== undefined) return;
		const base = Math.min(15000, 500 * 2 ** this.#attempt);
		const delay = base + Math.random() * 300;
		this.#attempt = Math.min(this.#attempt + 1, 6);
		this.#timer = window.setTimeout(() => {
			this.#timer = undefined;
			this.connect();
		}, delay);
	}
}

/** App-wide socket: list, report and split views all observe this one connection. */
export const appSocket = new LiveSocket("/api/ws");
