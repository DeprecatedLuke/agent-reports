<script lang="ts">
import { getBody, mintViewCap } from "../lib/api";
import ReportFrame from "./ReportFrame.svelte";

let { project, slug, rev }: { project: string; slug: string; rev: number } = $props();

let src = $state("");
let cap = $state<{ cap: string; expires_at: string } | null>(null);
let nonce = $state(0);
let error = $state<string | null>(null);

async function refresh(): Promise<string> {
	const next = await mintViewCap(project, slug);
	cap = next;
	error = null;
	const url = `/api/c/${next.cap}/?n=${++nonce}`;
	src = url;
	return url;
}

function fail(err: unknown): void {
	error = err instanceof Error ? err.message : String(err);
}

$effect(() => {
	// Re-mint whenever the component is pointed at another report.
	project;
	slug;
	void refresh().catch(fail);
});

// Mint a fresh capability shortly before the current one expires.
$effect(() => {
	const current = cap;
	if (!current) return;
	const expiresAt = Date.parse(current.expires_at);
	const delay = Math.max(5_000, expiresAt - Date.now() - 60_000);
	const timer = window.setTimeout(() => void refresh().catch(fail), delay);
	return () => window.clearTimeout(timer);
});

async function reloadSrc(): Promise<string> {
	return await refresh();
}

async function loadBody(): Promise<string> {
	return await getBody(project, slug);
}
</script>

{#if error && !src}
	<div class="surface-error" role="alert">
		<span>Could not load the document: {error}</span>
		<button type="button" class="btn" onclick={() => void refresh().catch(fail)}>Retry</button>
	</div>
{:else if src}
	<ReportFrame {src} {rev} {loadBody} {reloadSrc} title={`${project}: ${slug}`} />
{:else}
	<div class="surface-loading" aria-live="polite">Preparing document…</div>
{/if}

<style>
	.surface-error,
	.surface-loading {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 12px;
		height: 100%;
		padding: 24px;
		color: var(--muted);
		font-size: 14px;
	}

	.surface-error {
		color: var(--err);
	}
</style>
