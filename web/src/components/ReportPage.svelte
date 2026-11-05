<script lang="ts">
import { ApiError, deleteReport, getReport, listAssets, mintViewCap, type ReportDetail, reportKey } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { router, splitUrl } from "../lib/router.svelte";
import { appSocket } from "../lib/ws.svelte";
import Icon from "./Icon.svelte";
import ReportSurface from "./ReportSurface.svelte";
import ShareDialog from "./ShareDialog.svelte";

let { project, slug }: { project: string; slug: string } = $props();

let detail = $state<ReportDetail | null>(null);
let notFound = $state(false);
let deleted = $state(false);
let error = $state<string | null>(null);
let shareOpen = $state(false);
let deleteOpen = $state(false);
let deleteDialog = $state<HTMLDialogElement | undefined>(undefined);
let assetCount = $state<number | null>(null);
let deleting = $state(false);
let deleteError = $state<string | null>(null);
let copyState = $state<"idle" | "copied" | "failed">("idle");
let copyTimer: number | undefined;

const key = $derived(`${project}/${slug}`);

// The document inside the frame already shows project, title, description, updated time and rev.
$effect(() => {
	document.title = detail ? `${project}: ${detail.title}` : "agent-reports";
});

async function load(): Promise<void> {
	try {
		detail = await getReport(project, slug);
		notFound = false;
		deleted = false;
		error = null;
	} catch (err) {
		if (err instanceof ApiError && err.status === 404) notFound = true;
		else error = err instanceof Error ? err.message : String(err);
	}
}

$effect(() => {
	void load();
	const unsubscribe = appSocket.subscribe(event => {
		if (event.type === "report.upserted" && event.report) {
			if (reportKey(event.report) !== key) return;
			detail = detail ? { ...detail, ...event.report } : detail;
			deleted = false;
		} else if (event.type === "report.deleted") {
			if (event.project === project && event.slug === slug) deleted = true;
		} else if (event.type === "resync") {
			void load();
		}
	});
	const unsubscribeReconnect = appSocket.onReconnect(() => void load());
	appSocket.connect();
	return () => {
		unsubscribe();
		unsubscribeReconnect();
		if (copyTimer !== undefined) window.clearTimeout(copyTimer);
	};
});

async function copyReportUrl(): Promise<void> {
	const ok = await copyText(`report://${project}/${slug}`);
	copyState = ok ? "copied" : "failed";
	if (copyTimer !== undefined) window.clearTimeout(copyTimer);
	copyTimer = window.setTimeout(() => {
		copyState = "idle";
	}, 1800);
}

async function openDocument(): Promise<void> {
	try {
		const cap = await mintViewCap(project, slug);
		window.open(`/api/c/${cap.cap}/`, "_blank", "noopener");
	} catch (err) {
		error = err instanceof Error ? err.message : String(err);
	}
}

/** Open the confirmation with the asset count, so the cost of deleting is visible. */
async function askDelete(): Promise<void> {
	deleteError = null;
	assetCount = null;
	deleteOpen = true;
	try {
		assetCount = (await listAssets(project, slug)).length;
	} catch {
		// The count is a nicety; the dialog still works without it.
	}
}

$effect(() => {
	const element = deleteDialog;
	if (!element) return;
	if (deleteOpen && !element.open) element.showModal();
	else if (!deleteOpen && element.open) element.close();
});

async function confirmDelete(): Promise<void> {
	deleting = true;
	deleteError = null;
	try {
		await deleteReport(project, slug);
		deleteOpen = false;
		router.navigate("/");
	} catch (err) {
		deleteError = err instanceof Error ? err.message : String(err);
	} finally {
		deleting = false;
	}
}
</script>

{#if notFound}
	<div class="notice-card">
		<h1>Report not found</h1>
		<p><span class="mono">{key}</span> does not exist.</p>
		<a class="btn" href="/"><Icon name="arrow-left" size={14} /> All reports</a>
	</div>
{:else}
	<div class="report-page">
		<header class="bar">
			<a class="icon-btn" href="/" aria-label="Back to all reports" title="All reports">
				<Icon name="arrow-left" />
			</a>
			<div class="crumb">
				<span class="project">{project}</span>
				<span class="sep">/</span>
				<span class="slug mono">{slug}</span>
			</div>
			<div class="grow"></div>
			<div class="actions">
				<button type="button" class="btn" onclick={() => (shareOpen = true)} disabled={!detail}>
					<Icon name="share" size={14} /> Share
				</button>
				<a class="btn" href={splitUrl([key])}><Icon name="split" size={14} /> Open in split</a>
				<button type="button" class="btn" onclick={copyReportUrl}>
					<Icon name={copyState === "copied" ? "check" : "copy"} size={14} />
					{copyState === "copied" ? "Copied" : copyState === "failed" ? "Copy failed" : "Copy report://"}
				</button>
				<button type="button" class="btn" onclick={openDocument}>
					<Icon name="external" size={14} /> <span class="wide">Open document</span>
				</button>
				<button type="button" class="btn danger" onclick={askDelete} disabled={!detail} title="Delete this report">
					<Icon name="trash" size={14} /> <span class="wide">Delete</span>
				</button>
			</div>
		</header>

		{#if error}
			<div class="banner" role="alert">{error}</div>
		{/if}

		<div class="body">
			{#if deleted}
				<div class="notice-card inline">
					<h2>This report was deleted</h2>
					<p><span class="mono">{key}</span> is no longer available.</p>
					<a class="btn" href="/">Back to all reports</a>
				</div>
			{:else if detail}
				<ReportSurface {project} {slug} rev={detail.rev} />
			{:else}
				<div class="empty" aria-live="polite">Loading…</div>
			{/if}
		</div>

		<ShareDialog bind:open={shareOpen} {project} {slug} />

		<dialog bind:this={deleteDialog} class="delete-dialog" aria-labelledby="delete-dialog-title">
			<div class="inner">
				<div class="head">
					<h2 id="delete-dialog-title">Delete report</h2>
					<button
						type="button"
						class="icon-btn"
						aria-label="Cancel"
						onclick={() => (deleteOpen = false)}
					>
						<Icon name="close" />
					</button>
				</div>
				<p class="hint">
					<span class="mono">{key}</span> and everything attached to it are removed:
					{#if assetCount !== null && assetCount > 0}
						{assetCount}
						{assetCount === 1 ? "asset" : "assets"} and its open share links.
					{:else}
						its assets and its open share links.
					{/if}
					This cannot be undone.
				</p>
				{#if deleteError}
					<p class="error" role="alert">{deleteError}</p>
				{/if}
				<div class="confirm-row">
					<button type="button" class="btn" onclick={() => (deleteOpen = false)}>Cancel</button>
					<button type="button" class="btn danger" onclick={confirmDelete} disabled={deleting}>
						<Icon name="trash" size={14} />
						{deleting ? "Deleting…" : "Delete report"}
					</button>
				</div>
			</div>
		</dialog>
	</div>
{/if}

<style>
	.report-page {
		display: flex;
		flex-direction: column;
		min-height: 0;
		height: 100%;
	}

	.bar {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 9px 18px;
		border-bottom: 1px solid var(--border);
		flex: none;
	}

	.crumb {
		display: flex;
		gap: 3px;
		min-width: 0;
		font-size: 12.5px;
		color: var(--muted);
	}

	.crumb .project {
		font-weight: 650;
		color: var(--accent);
	}

	.crumb .slug {
		font-size: 12px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.sep {
		opacity: 0.5;
	}

	.grow {
		flex: 1;
	}

	.actions {
		display: flex;
		gap: 7px;
		flex-wrap: wrap;
		justify-content: flex-end;
	}

	.banner {
		padding: 8px 18px;
		border-bottom: 1px solid var(--border);
		color: var(--err);
		background: light-dark(#fff5f5, #2a1618);
		font-size: 13px;
	}

	.body {
		flex: 1;
		min-height: 0;
		position: relative;
	}

	.empty {
		padding: 60px 20px;
		text-align: center;
		color: var(--muted);
	}

	.notice-card {
		max-width: 520px;
		margin: 80px auto;
		padding: 26px;
		border: 1px solid var(--border);
		border-radius: 14px;
		background: var(--surface);
		text-align: center;
	}

	.notice-card.inline {
		margin: 80px auto;
	}

	.notice-card h1,
	.notice-card h2 {
		font-size: 18px;
		margin-bottom: 8px;
	}

	.notice-card p {
		color: var(--muted);
		font-size: 13.5px;
		margin: 0 0 16px;
	}

	.delete-dialog {
		width: min(520px, calc(100vw - 32px));
		padding: 0;
		border: 1px solid var(--border);
		border-radius: 14px;
		background: var(--bg);
		color: var(--fg);
		box-shadow: var(--shadow);
	}

	.delete-dialog::backdrop {
		background: light-dark(rgb(15 20 30 / 35%), rgb(0 0 0 / 62%));
		backdrop-filter: blur(2px);
	}

	.delete-dialog .inner {
		padding: 16px 18px 18px;
	}

	.delete-dialog .head {
		display: flex;
		align-items: center;
		gap: 12px;
	}

	.delete-dialog .head h2 {
		flex: 1;
		font-size: 16.5px;
	}

	.delete-dialog .hint {
		margin: 6px 0 14px;
		color: var(--muted);
		font-size: 13px;
	}

	.delete-dialog .error {
		margin: 0 0 12px;
		color: var(--err);
		font-size: 13px;
	}

	.confirm-row {
		display: flex;
		gap: 8px;
		justify-content: flex-end;
	}

	@media (max-width: 720px) {
		.bar {
			flex-wrap: wrap;
		}

		.actions {
			justify-content: flex-start;
			width: 100%;
		}

		.wide {
			display: none;
		}
	}
</style>
