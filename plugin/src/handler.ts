/**
 * `report://` protocol handler for the agent-reports service.
 *
 * URL forms:
 * - `report://` / `report://list[/<project>]` / `report://list?project=*` — listings.
 * - `report://guide` — the writing guide (`agent-reports/guide`, text version).
 * - `report://<project>/<slug>` — the editable source. Reads resolve to a local
 *   mirror file so hashline anchors and `edit` work; every change is pushed back
 *   by the plugin's `tool_result` hook (see `pushPendingEdits`).
 * - `report://<project>/<slug>.md` — the server's text version (immutable).
 *
 * Writes go through `write` (PUT with optimistic concurrency on the rev last
 * observed in this process).
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import type {
	InternalResource,
	InternalUrl,
	InternalWriteResult,
	ProtocolHandler,
	ResolveContext,
	SchemeSpec,
	UrlCompletion,
	WriteContext,
} from "@oh-my-pi/pi-coding-agent/internal-urls";
import {
	type AgentReportsConfig,
	ApiError,
	fetchSource,
	fetchTextVersion,
	IDENTITY_RE,
	listProjects,
	listReports,
	MAX_SOURCE_BYTES,
	type PutReportResult,
	putSource,
	type ReportSummary,
	readConfig,
	validationProblems,
} from "./api";

/** Canonical cache directory for report mirrors. */
export function mirrorRoot(): string {
	const base = process.env.XDG_CACHE_HOME?.trim() || path.join(os.homedir(), ".cache");
	return path.join(base, "omp-agent-reports");
}

/** Local file backing `report://<project>/<slug>` (also its edit target). */
export function mirrorPath(project: string, slug: string): string {
	return path.join(mirrorRoot(), project, `${slug}.md`);
}

/** One report observed in this process, with the rev last synced from the server. */
export interface ReportState {
	project: string;
	slug: string;
	mirror: string;
	rev?: number;
	/** Source text the server held when `mirror` was last written. */
	syncedText?: string;
}

/** Live report state, keyed by `<project>/<slug>`. */
export const reportStates = new Map<string, ReportState>();

/** Report catalogue memo for URL completion (keystrokes must not hit the network). */
const COMPLETION_TTL_MS = 30_000;
let completionCache: { url: string; at: number; entries: ReportSummary[] } | undefined;

/** Lowercased project name derived from a working directory. */
export function projectFromCwd(cwd: string): string {
	const base = path
		.basename(path.resolve(cwd))
		.toLowerCase()
		.replace(/[^a-z0-9_-]+/g, "-");
	const trimmed = base
		.replace(/^[^a-z0-9]+/, "")
		.replace(/-+$/, "")
		.slice(0, 64)
		.replace(/-+$/, "");
	return trimmed === "" ? "project" : trimmed;
}

/** ENOENT-coded error, so the read tool treats a missing report as "not found". */
function reportNotFound(project: string, slug: string): Error {
	return Object.assign(new Error(`report://${project}/${slug} does not exist`), { code: "ENOENT" });
}

/** Model-facing error for a failed PUT. */
export function saveError(error: unknown, project: string, slug: string): Error {
	if (!(error instanceof Error)) return new Error(String(error));
	if (!(error instanceof ApiError)) return error;
	switch (error.status) {
		case 401:
			return new Error(error.message);
		case 412:
			return new Error(`report changed (or already exists) — read report://${project}/${slug} again`);
		case 413:
			return new Error(`report too large — the server rejects source bodies over ${MAX_SOURCE_BYTES / 1048576} MiB`);
		case 422: {
			const problems = validationProblems(error);
			if (problems.length === 0) return new Error(`report://${project}/${slug} rejected (422): ${error.message}`);
			const lines = problems.map(problem =>
				problem.line === undefined ? `- ${problem.message}` : `- line ${problem.line}: ${problem.message}`,
			);
			return new Error(`report://${project}/${slug} rejected (422):\n${lines.join("\n")}`);
		}
		default:
			return new Error(`cannot save report://${project}/${slug}: ${error.message}`);
	}
}

type ParsedReportUrl =
	| { kind: "list"; project?: string }
	| { kind: "guide" }
	| { kind: "source"; project: string; slug: string }
	| { kind: "text"; project: string; slug: string }
	| { kind: "invalid"; message: string };

function identityError(value: string, label: string): string {
	return `report:// ${label} "${value}" must match ${IDENTITY_RE.source} (lowercase letters, digits, "_" and "-")`;
}

/** Parse a `report://` URL into the endpoint it addresses. */
export function parseReportUrl(url: InternalUrl): ParsedReportUrl {
	const host = (url.rawHost || url.hostname || "").toLowerCase();
	const rawPath = url.pathname.replace(/^\/+/, "");
	const pathSegments = rawPath === "" ? [] : rawPath.split("/");
	const projectQuery = url.searchParams.get("project");

	if (host === "" && pathSegments.length === 0) return { kind: "list" };
	if (host === "list") {
		if (pathSegments.length > 1) {
			return {
				kind: "invalid",
				message: "report://list takes at most one project name: report://list?project=<name>",
			};
		}
		const selected = pathSegments[0] ?? projectQuery ?? undefined;
		if (selected === undefined || selected === "") return { kind: "list" };
		if (selected === "*") return { kind: "list", project: "*" };
		if (!IDENTITY_RE.test(selected)) return { kind: "invalid", message: identityError(selected, "project") };
		return { kind: "list", project: selected };
	}
	if (host === "guide") {
		if (pathSegments.length > 0) {
			return { kind: "invalid", message: "report://guide takes no path; read report://guide" };
		}
		return { kind: "guide" };
	}
	if (!IDENTITY_RE.test(host)) return { kind: "invalid", message: identityError(host, "project") };
	if (pathSegments.length === 0) return { kind: "list", project: host };
	if (pathSegments.length > 1) {
		return { kind: "invalid", message: `report:// URLs address exactly one report: report://${host}/<slug>` };
	}
	const rawSlug = pathSegments[0] ?? "";
	if (rawSlug.endsWith(".md") && rawSlug.length > 3) {
		const slug = rawSlug.slice(0, -3);
		if (!IDENTITY_RE.test(slug)) return { kind: "invalid", message: identityError(slug, "slug") };
		return { kind: "text", project: host, slug };
	}
	if (!IDENTITY_RE.test(rawSlug)) return { kind: "invalid", message: identityError(rawSlug, "slug") };
	return { kind: "source", project: host, slug: rawSlug };
}

/**
 * Mirror file backing a `report://<project>/<slug>` source URL, or undefined
 * for listings, the guide and text versions. An `edit` on the URL is a hashline
 * patch of that file, so this is what the push step must look for.
 */
export function reportUrlToMirror(input: string): string | undefined {
	if (!input.startsWith("report://")) return undefined;
	const [pathname] = input.slice("report://".length).split(/[?#]/);
	const segments = (pathname ?? "").split("/");
	if (segments.length !== 2) return undefined;
	const project = (segments[0] ?? "").toLowerCase();
	const slug = (segments[1] ?? "").toLowerCase();
	if (project === "list" || project === "guide") return undefined;
	if (slug.endsWith(".md")) return undefined;
	if (!IDENTITY_RE.test(project) || !IDENTITY_RE.test(slug)) return undefined;
	return mirrorPath(project, slug);
}

function markdown(url: InternalUrl, text: string, immutable: boolean): InternalResource {
	return {
		url: url.href,
		content: text,
		contentType: "text/markdown",
		size: Buffer.byteLength(text, "utf8"),
		immutable,
	};
}

function formatTime(value: string): string {
	const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(value);
	return match === null ? value : `${match[1]} ${match[2]}Z`;
}

function reportLine(report: ReportSummary): string {
	const parts = [`report://${report.project}/${report.slug}`, report.title];
	if (report.description.trim() !== "") parts.push(report.description.trim().replace(/\s+/g, " "));
	parts.push(`updated ${formatTime(report.updated_at)}`);
	parts.push(`rev ${report.rev}`);
	return `- ${parts.join(" · ")}`;
}

function plurals(count: number, one: string, many: string): string {
	return `${count} ${count === 1 ? one : many}`;
}

function otherProjectsLine(projects: readonly { project: string; count: number }[], current: string): string {
	const others = projects.filter(entry => entry.project !== current);
	if (others.length === 0) return "No other projects have reports.";
	const listed = others.map(entry => `${entry.project} (${entry.count})`).join(", ");
	return `Other projects: ${listed} — view one with \`read report://list?project=<name>\`, all with \`read report://list?project=*\`.`;
}

/** Render the report listing for one project, or every project for `"*"`. */
export async function renderListing(
	cfg: AgentReportsConfig,
	project: string | "*" | undefined,
	cwd: string,
	signal?: AbortSignal,
): Promise<string> {
	const projects = await listProjects(cfg, signal).catch(() => []);
	if (project === "*") {
		const reports = await listReports(cfg, undefined, signal);
		const grouped = new Map<string, ReportSummary[]>();
		for (const report of reports) {
			const bucket = grouped.get(report.project);
			if (bucket === undefined) grouped.set(report.project, [report]);
			else bucket.push(report);
		}
		const lines = ["# All agent-reports reports", ""];
		if (reports.length === 0) {
			lines.push("No reports exist yet. Create one: `write report://<project>/<slug>`.");
		}
		for (const name of [...grouped.keys()].sort((left, right) => left.localeCompare(right))) {
			const bucket = grouped.get(name) ?? [];
			lines.push(`## ${name} (${bucket.length})`, "");
			for (const report of bucket) lines.push(reportLine(report));
			lines.push("");
		}
		lines.push(
			`${plurals(reports.length, "report", "reports")} across ${plurals(grouped.size, "project", "projects")}.`,
		);
		return `${lines.join("\n").trimEnd()}\n`;
	}

	const selected = project ?? projectFromCwd(cwd);
	const reports = await listReports(cfg, selected, signal);
	const lines = [`# Reports in ${selected}`, ""];
	if (reports.length === 0) {
		lines.push(
			`No reports in \`${selected}\` yet. Create one with \`write report://${selected}/<slug>\` (frontmatter \`title\` required).`,
		);
	} else {
		for (const report of reports) lines.push(reportLine(report));
		lines.push("", `${plurals(reports.length, "report", "reports")}.`);
	}
	lines.push("", otherProjectsLine(projects, selected));
	return `${lines.join("\n").trimEnd()}\n`;
}

/**
 * Fetch the source of one report into its local mirror and remember the rev.
 * Returns null when the report does not exist.
 */
export async function syncMirror(
	cfg: AgentReportsConfig,
	project: string,
	slug: string,
	signal?: AbortSignal,
): Promise<ReportState | null> {
	const snapshot = await fetchSource(cfg, project, slug, signal);
	const key = `${project}/${slug}`;
	if (snapshot === null) {
		reportStates.delete(key);
		return null;
	}
	const mirror = mirrorPath(project, slug);
	await fs.mkdir(path.dirname(mirror), { recursive: true });
	const current = await fs.readFile(mirror, "utf8").catch(() => undefined);
	if (current !== snapshot.text) await fs.writeFile(mirror, snapshot.text, "utf8");
	const state: ReportState = { project, slug, mirror, rev: snapshot.rev, syncedText: snapshot.text };
	reportStates.set(key, state);
	return state;
}

/** One report pushed back after an editor wrote its mirror. */
export interface SavedReport {
	url: string;
	rev: number;
	publicUrl: string;
}

/** Result of pushing pending mirror edits back to the service. */
export interface EditPushOutcome {
	saved: SavedReport[];
	failed?: { url: string; error: Error };
}

/**
 * Push the mirror or mirrors an editor just wrote back to the service. Runs
 * after `edit` or `write` tool results; stops at the first failure so the model
 * sees why.
 *
 * Only files named in `editedPaths` are considered: mirrors live in a shared
 * cache directory, so an unfiltered push would republish another session's (or
 * a stale) mirror after an unrelated write.
 */
export async function pushPendingEdits(
	cwd: string,
	editedPaths?: readonly string[],
	signal?: AbortSignal,
): Promise<EditPushOutcome> {
	if (reportStates.size === 0) return { saved: [] };
	const targets = editedPaths === undefined ? undefined : new Set(editedPaths.map(file => path.resolve(cwd, file)));
	const states =
		targets === undefined
			? [...reportStates.values()]
			: [...reportStates.values()].filter(state => targets.has(path.resolve(state.mirror)));
	if (states.length === 0) return { saved: [] };
	const cfg = await readConfig(cwd);
	const saved: SavedReport[] = [];
	for (const state of states) {
		const text = await fs.readFile(state.mirror, "utf8").catch(() => undefined);
		if (text === undefined || text === state.syncedText) continue;
		try {
			const result = await putSource(
				cfg,
				state.project,
				state.slug,
				text,
				state.rev === undefined ? { ifNoneMatch: true } : { ifMatch: state.rev },
				signal,
			);
			state.rev = result.report.rev;
			state.syncedText = text;
			saved.push({
				url: `report://${state.project}/${state.slug}`,
				rev: result.report.rev,
				publicUrl: result.url,
			});
		} catch (error) {
			// The mirror no longer matches the server: drop the tracked state so the
			// next read re-syncs instead of retrying this unsaved content.
			reportStates.delete(`${state.project}/${state.slug}`);
			return {
				saved,
				failed: {
					url: `report://${state.project}/${state.slug}`,
					error: saveError(error, state.project, state.slug),
				},
			};
		}
	}
	return { saved };
}

/** `report://` handler: listings, guide, source (read/write/edit), text version. */
export class AgentReportsHandler implements ProtocolHandler {
	readonly scheme = "report";
	readonly spec: SchemeSpec = {
		backing: "file",
		selectors: "lines",
		immutable: false,
		write: { via: "handler", fileEdits: true, payload: "text", scope: "workspace", tier: () => "write" },
	};

	async resolve(url: InternalUrl, context?: ResolveContext): Promise<InternalResource> {
		const parsed = parseReportUrl(url);
		const cwd = context?.cwd ?? process.cwd();
		if (parsed.kind === "invalid") throw new Error(parsed.message);
		const cfg = await readConfig(cwd);
		switch (parsed.kind) {
			case "list":
				return markdown(url, await renderListing(cfg, parsed.project, cwd, context?.signal), true);
			case "guide": {
				const text = await fetchTextVersion(cfg, "agent-reports", "guide", context?.signal);
				if (text === null) throw new Error("the agent-reports guide is not published yet (agent-reports/guide)");
				return markdown(url, text, true);
			}
			case "text": {
				const text = await fetchTextVersion(cfg, parsed.project, parsed.slug, context?.signal);
				if (text === null) throw reportNotFound(parsed.project, parsed.slug);
				return markdown(url, text, true);
			}
			case "source": {
				const state = await syncMirror(cfg, parsed.project, parsed.slug, context?.signal);
				if (state === null) throw reportNotFound(parsed.project, parsed.slug);
				return {
					url: url.href,
					content: state.syncedText ?? "",
					contentType: "text/markdown",
					size: Buffer.byteLength(state.syncedText ?? "", "utf8"),
					sourcePath: state.mirror,
					immutable: false,
				};
			}
		}
	}

	/**
	 * Local mirror of the source, so `read` gets hashline anchors and `edit`
	 * patches a real file. Only text-consumable forms have a backing file.
	 */
	async locate(
		url: InternalUrl,
		context?: ResolveContext,
		options?: { localOnly?: boolean; create?: boolean },
	): Promise<string | null> {
		const parsed = parseReportUrl(url);
		if (parsed.kind !== "source") return null;
		// `localOnly` callers (`%load`, plan checks, `:img`) must not trigger a network read.
		if (options?.localOnly === true) return null;
		const cfg = await readConfig(context?.cwd ?? process.cwd());
		const state = await syncMirror(cfg, parsed.project, parsed.slug, context?.signal);
		if (state !== null) return state.mirror;
		if (options?.create === true) {
			throw new Error(`report://${parsed.project}/${parsed.slug} does not exist — create it with the write tool`);
		}
		return null;
	}

	async write(url: InternalUrl, content: string, context?: WriteContext): Promise<InternalWriteResult> {
		const parsed = parseReportUrl(url);
		if (parsed.kind === "invalid") throw new Error(parsed.message);
		if (parsed.kind !== "source") {
			throw new Error(
				`${url.href} is read-only; write the source with report://<project>/<slug> (no .md suffix, not a listing)`,
			);
		}
		const { project, slug } = parsed;
		const cwd = context?.cwd ?? process.cwd();
		const cfg = await readConfig(cwd);
		const state = reportStates.get(`${project}/${slug}`);
		const precondition = state?.rev === undefined ? { ifNoneMatch: true as const } : { ifMatch: state.rev };
		let result: PutReportResult;
		try {
			result = await putSource(cfg, project, slug, content, precondition, context?.signal);
		} catch (error) {
			throw saveError(error, project, slug);
		}
		const mirror = state?.mirror ?? mirrorPath(project, slug);
		await fs.mkdir(path.dirname(mirror), { recursive: true });
		await fs.writeFile(mirror, content, "utf8");
		reportStates.set(`${project}/${slug}`, {
			project,
			slug,
			mirror,
			rev: result.report.rev,
			syncedText: content,
		});
		return {
			content: [
				{
					type: "text",
					text: `Saved report://${project}/${slug} rev ${result.report.rev} — ${result.url}`,
				},
			],
			details: { resolvedPath: mirror },
		};
	}

	async complete(_query?: string, context?: ResolveContext): Promise<UrlCompletion[]> {
		const cfg = await readConfig(context?.cwd ?? process.cwd());
		const items: UrlCompletion[] = [
			{ value: "list", description: "reports in this project" },
			{ value: "list?project=*", description: "every project" },
			{ value: "guide", description: "how to write reports" },
		];
		const now = Date.now();
		let catalogue = completionCache;
		if (catalogue === undefined || catalogue.url !== cfg.url || now - catalogue.at > COMPLETION_TTL_MS) {
			try {
				const reports = await listReports(cfg, undefined, context?.signal);
				catalogue = { url: cfg.url, at: now, entries: reports };
				completionCache = catalogue;
			} catch {
				// Completion is best-effort: an offline service must not break typing.
				catalogue = undefined;
			}
		}
		for (const report of catalogue?.entries ?? []) {
			items.push({
				value: `${report.project}/${report.slug}`,
				label: `${report.project}/${report.slug}`,
				description: report.title,
			});
		}
		return items;
	}
}
