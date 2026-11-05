<script lang="ts">
import { ApiError, getShareBody, getShareMeta, type ShareMeta } from "../lib/api";
import { exactTime, expiresIn, isExpired, startClock } from "../lib/time";
import { LiveSocket, type WsEvent } from "../lib/ws.svelte";
import ReportFrame from "./ReportFrame.svelte";
import ThemeToggle from "./ThemeToggle.svelte";

function readToken(): string {
	const match = location.pathname.match(/^\/s\/([^/]+)\/?$/);
	return match ? decodeURIComponent(match[1]) : "";
}

const token = readToken();

let meta = $state<ShareMeta | null>(null);
let view = $state<"loading" | "ready" | "expired" | "notfound" | "error">("loading");
let error = $state<string | null>(null);
let rev = $state(0);
let deleted = $state(false);
let nonce = $state(0);
let now = $state(Date.now());

const socket = new LiveSocket(`/api/s/${encodeURIComponent(token)}/ws`);

// The frame document shows project, title and description; the tab title is not on the page.
$effect(() => {
	document.title = meta ? `${meta.project}: ${meta.title}` : "shared report · agent-reports";
});

async function loadMeta(): Promise<void> {
	if (!token) {
		view = "notfound";
		return;
	}
	try {
		const next = await getShareMeta(token);
		meta = next;
		rev = next.rev;
		error = null;
		view = isExpired(next.expires_at, Date.now()) ? "expired" : "ready";
	} catch (err) {
		if (err instanceof ApiError && (err.status === 410 || err.status === 404)) {
			// The shell loads for any token; stop reconnecting to a share that does not exist.
			view = err.status === 410 ? "expired" : "notfound";
			socket.close();
		} else {
			view = "error";
			error = err instanceof Error ? err.message : String(err);
		}
	}
}

async function loadBody(): Promise<string> {
	try {
		return await getShareBody(token);
	} catch (err) {
		if (err instanceof ApiError && err.status === 410) view = "expired";
		throw err;
	}
}

async function reloadSrc(): Promise<string> {
	// The share token is itself the document capability; only bust the frame cache.
	return `/api/c/${encodeURIComponent(token)}/?n=${++nonce}`;
}

$effect(() => {
	void loadMeta();
	const unsubscribe = socket.subscribe((event: WsEvent) => {
		if (event.type === "report.upserted" && event.report) {
			rev = event.report.rev;
			if (meta) {
				meta = {
					...meta,
					title: event.report.title,
					description: event.report.description,
					rev: event.report.rev,
					updated_at: event.report.updated_at,
				};
			}
		} else if (event.type === "report.deleted") {
			deleted = true;
		} else if (event.type === "share.expired") {
			view = "expired";
			socket.close();
		}
	});
	const unsubscribeReconnect = socket.onReconnect(() => void loadMeta());
	socket.connect();
	const stopClock = startClock(15_000, tick => {
		now = tick;
		if (meta && view === "ready" && isExpired(meta.expires_at, tick)) view = "expired";
	});
	return () => {
		unsubscribe();
		unsubscribeReconnect();
		stopClock();
		socket.close();
	};
});

const src = $derived(token && view === "ready" ? `/api/c/${encodeURIComponent(token)}/?n=${nonce}` : "");
</script>

<div class="share-page">
	<header class="bar">
		<span class="grow"></span>
		<div class="actions">
			{#if meta && view === "ready" && !isExpired(meta.expires_at, now)}
				<span class="expiry" title={exactTime(meta.expires_at)}>{expiresIn(meta.expires_at, now)}</span>
			{/if}
			<ThemeToggle />
		</div>
	</header>

	<div class="body">
		{#if view === "expired"}
			<div class="notice">
				<h2>This link has expired</h2>
				<p>Ask the report owner for a new share link.</p>
			</div>
		{:else if view === "notfound"}
			<div class="notice">
				<h2>Link not found</h2>
				<p>This share link is invalid or was revoked.</p>
			</div>
		{:else if view === "error"}
			<div class="notice">
				<h2>Could not load the report</h2>
				<p role="alert">{error}</p>
			</div>
		{:else if view === "loading"}
			<div class="notice">
				<p>Loading…</p>
			</div>
		{:else if deleted}
			<div class="notice">
				<h2>This report is no longer available</h2>
			</div>
		{:else if src}
			<ReportFrame
				{src}
				{rev}
				loadBody={loadBody}
				reloadSrc={reloadSrc}
				title={meta ? `${meta.project}: ${meta.title}` : "Shared report"}
			/>
		{/if}
	</div>
</div>

<style>
	.share-page {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
	}

	.bar {
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 8px 18px;
		border-bottom: 1px solid var(--border);
		flex: none;
	}

	.grow {
		flex: 1;
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 10px;
		flex: none;
	}

	.expiry {
		font-size: 12.5px;
		color: var(--muted);
		white-space: nowrap;
	}

	.body {
		flex: 1;
		min-height: 0;
		position: relative;
	}

	.notice {
		max-width: 460px;
		margin: 90px auto;
		padding: 26px;
		border: 1px solid var(--border);
		border-radius: 14px;
		background: var(--surface);
		text-align: center;
	}

	.notice h2 {
		font-size: 18px;
		margin-bottom: 8px;
	}

	.notice p {
		margin: 0;
		color: var(--muted);
		font-size: 13.5px;
	}
</style>
