<script lang="ts">
import { getProjects, listReports, type Project, type ReportSummary, reportKey } from "../lib/api";
import { router, splitUrl } from "../lib/router.svelte";
import { exactTime, relativeTime, startClock } from "../lib/time";
import { appSocket } from "../lib/ws.svelte";
import Icon from "./Icon.svelte";

let reports = $state<ReportSummary[]>([]);
let projects = $state<Project[]>([]);
let loading = $state(true);
let error = $state<string | null>(null);
let projectFilter = $state<string | null>(null);
let query = $state("");
let highlighted = $state<string | null>(null);
let now = $state(Date.now());
let highlightTimer: number | undefined;

const filtered = $derived.by(() => {
	const needle = query.trim().toLowerCase();
	return reports.filter(report => {
		if (projectFilter && report.project !== projectFilter) return false;
		if (!needle) return true;
		return (
			report.title.toLowerCase().includes(needle) ||
			report.description.toLowerCase().includes(needle) ||
			report.project.toLowerCase().includes(needle) ||
			`${report.project}/${report.slug}`.includes(needle)
		);
	});
});

const PAGE_SIZE = 25;
const pageCount = $derived(Math.max(1, Math.ceil(filtered.length / PAGE_SIZE)));
// `?page=N` (1-based) keeps the position across back/forward; out-of-range values clamp.
const page = $derived.by(() => {
	const requested = Number.parseInt(router.url.searchParams.get("page") ?? "1", 10);
	return Number.isFinite(requested) ? Math.min(Math.max(requested, 1), pageCount) : 1;
});
const visible = $derived(filtered.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE));

function goToPage(next: number): void {
	router.navigate(next <= 1 ? "/" : `/?page=${next}`);
}

/** A new search or project filter starts from the first page. */
function resetPage(): void {
	if (router.url.searchParams.has("page")) router.navigate("/", { replace: true });
}

function selectProject(project: string | null): void {
	projectFilter = project;
	resetPage();
}

function sortReports(list: ReportSummary[]): ReportSummary[] {
	return [...list].sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
}

async function refresh(): Promise<void> {
	try {
		const [nextReports, nextProjects] = await Promise.all([listReports(), getProjects()]);
		reports = nextReports;
		projects = nextProjects;
		error = null;
	} catch (err) {
		error = err instanceof Error ? err.message : String(err);
	} finally {
		loading = false;
	}
}

function flash(key: string): void {
	highlighted = key;
	if (highlightTimer !== undefined) window.clearTimeout(highlightTimer);
	highlightTimer = window.setTimeout(() => {
		highlighted = null;
		highlightTimer = undefined;
	}, 1600);
}

$effect(() => {
	void refresh();
	const unsubscribe = appSocket.subscribe(event => {
		if (event.type === "report.upserted" && event.report) {
			const incoming = event.report;
			const key = reportKey(incoming);
			const exists = reports.some(report => reportKey(report) === key);
			reports = sortReports(
				exists ? reports.map(report => (reportKey(report) === key ? incoming : report)) : [incoming, ...reports],
			);
			flash(key);
			if (!projects.some(project => project.project === incoming.project)) void refresh();
		} else if (event.type === "report.deleted") {
			reports = reports.filter(report => !(report.project === event.project && report.slug === event.slug));
			void refresh();
		} else if (event.type === "resync") {
			void refresh();
		}
	});
	const unsubscribeReconnect = appSocket.onReconnect(() => void refresh());
	appSocket.connect();
	const stopClock = startClock(30_000, tick => {
		now = tick;
	});
	return () => {
		unsubscribe();
		unsubscribeReconnect();
		stopClock();
		if (highlightTimer !== undefined) window.clearTimeout(highlightTimer);
	};
});
</script>

<div class="list-page">
	<div class="toolbar">
		<label class="search">
			<Icon name="search" size={15} />
			<input
				class="input"
				type="search"
				bind:value={query}
				oninput={resetPage}
				placeholder="Search reports…"
				aria-label="Search reports"
			/>
		</label>
		<div class="chips" role="group" aria-label="Filter by project">
			<button type="button" class="chip" aria-pressed={projectFilter === null} onclick={() => selectProject(null)}>
				All <span class="count">{reports.length}</span>
			</button>
			{#each projects as project (project.project)}
				<button
					type="button"
					class="chip"
					aria-pressed={projectFilter === project.project}
					onclick={() => selectProject(projectFilter === project.project ? null : project.project)}
				>
					{project.project} <span class="count">{project.count}</span>
				</button>
			{/each}
		</div>
	</div>

	{#if error}
		<div class="banner" role="alert">
			<span>{error}</span>
			<button type="button" class="btn" onclick={() => void refresh()}>Retry</button>
		</div>
	{/if}

	{#if loading && reports.length === 0}
		<div class="empty">Loading…</div>
	{:else if filtered.length === 0}
		<div class="empty">
			{#if reports.length === 0}
				<p>No reports yet.</p>
				<p class="hint">Agents publish with <span class="mono">report://project/slug</span>.</p>
			{:else}
				<p>No reports match this filter.</p>
			{/if}
		</div>
	{:else}
		<ul class="rows">
			{#each visible as report (reportKey(report))}
				{@const key = reportKey(report)}
				<li class="row" class:flash={highlighted === key}>
					<a class="row-main" href={`/r/${report.project}/${report.slug}`}>
						<span class="line top">
							<span class="project">{report.project}</span>
							<span class="sep">/</span>
							<span class="slug mono">{report.slug}</span>
							<span class="grow"></span>
							<time class="updated" datetime={report.updated_at} title={exactTime(report.updated_at)}>
								{relativeTime(report.updated_at, now)}
							</time>
							<span class="rev mono">rev {report.rev}</span>
						</span>
						<span class="title">{report.title}</span>
						{#if report.description}
							<span class="desc">{report.description}</span>
						{/if}
					</a>
					<div class="row-actions">
						<a
							class="icon-btn"
							href={splitUrl([key])}
							aria-label={`Open ${key} in split view`}
							title="Open in split view"
						>
							<Icon name="split" size={15} />
						</a>
					</div>
				</li>
			{/each}
		</ul>
		{#if pageCount > 1}
			<nav class="pager" aria-label="Pages">
				<button type="button" class="btn" disabled={page === 1} onclick={() => goToPage(page - 1)}>Previous</button>
				<span class="pager-status">
					Page {page} of {pageCount}
					<span class="pager-range">
						· {(page - 1) * PAGE_SIZE + 1}–{Math.min(page * PAGE_SIZE, filtered.length)} of {filtered.length}
					</span>
				</span>
				<button type="button" class="btn" disabled={page === pageCount} onclick={() => goToPage(page + 1)}>
					Next
				</button>
			</nav>
		{/if}
	{/if}
</div>

<style>
	.list-page {
		width: 100%;
		max-width: 1040px;
		margin: 0 auto;
		padding: 18px 20px 60px;
	}

	.toolbar {
		display: flex;
		align-items: center;
		gap: 14px;
		flex-wrap: wrap;
		margin-bottom: 16px;
	}

	.search {
		position: relative;
		display: flex;
		align-items: center;
		gap: 8px;
		flex: 1 1 260px;
		max-width: 360px;
		color: var(--muted);
	}

	.search .input {
		padding-left: 10px;
	}

	.search :global(svg) {
		position: absolute;
		left: 10px;
		pointer-events: none;
	}

	.search .input {
		padding-left: 32px;
	}

	.chips {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}

	.banner {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 9px 13px;
		margin-bottom: 14px;
		border: 1px solid var(--err);
		border-radius: 9px;
		color: var(--err);
		background: light-dark(#fff5f5, #2a1618);
		font-size: 13.5px;
	}

	.banner span {
		flex: 1;
	}

	.empty {
		padding: 60px 20px;
		text-align: center;
		color: var(--muted);
	}

	.empty p {
		margin: 6px 0;
	}

	.empty .hint {
		font-size: 13px;
	}

	.rows {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.row {
		display: flex;
		align-items: stretch;
		gap: 4px;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--bg);
		transition:
			border-color 0.12s ease,
			box-shadow 0.12s ease;
	}

	.row:hover {
		border-color: color-mix(in oklab, var(--border), var(--fg) 22%);
		box-shadow: 0 2px 10px light-dark(rgb(15 20 30 / 5%), rgb(0 0 0 / 25%));
	}

	.row.flash {
		animation: flash 1.4s ease-out;
	}

	@keyframes flash {
		0% {
			background: var(--accent-weak);
			border-color: var(--accent);
		}
		100% {
			background: var(--bg);
		}
	}

	.row-main {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: 11px 14px;
		color: var(--fg);
	}

	.row-main:hover {
		text-decoration: none;
	}

	.line {
		display: flex;
		align-items: baseline;
		gap: 8px;
		min-width: 0;
	}

	.top {
		font-size: 12px;
		color: var(--muted);
		margin-bottom: 1px;
	}

	.project {
		font-weight: 600;
		color: var(--accent);
	}

	.sep {
		opacity: 0.5;
	}

	.slug {
		font-size: 11.5px;
	}

	.grow {
		flex: 1;
	}

	.updated {
		white-space: nowrap;
	}

	.rev {
		font-size: 11px;
		opacity: 0.8;
		white-space: nowrap;
	}

	.title {
		font-weight: 600;
		font-size: 15.5px;
		letter-spacing: -0.01em;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.desc {
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		color: var(--muted);
		font-size: 13.5px;
	}

	.row-actions {
		display: flex;
		align-items: center;
		padding-right: 8px;
	}

	.pager {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 14px;
		margin-top: 18px;
	}

	.pager-status {
		font-size: 13px;
		color: var(--fg);
		font-variant-numeric: tabular-nums;
	}

	.pager-range {
		color: var(--muted);
	}
</style>
