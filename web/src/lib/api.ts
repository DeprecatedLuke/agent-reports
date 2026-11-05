export interface ReportSummary {
	project: string;
	slug: string;
	title: string;
	description: string;
	rev: number;
	created_at: string;
	updated_at: string;
	size: number;
}

export interface Project {
	project: string;
	count: number;
	updated_at: string;
}

export interface Share {
	token: string;
	url: string;
	created_at: string;
	expires_at: string;
}

export interface ReportDetail extends ReportSummary {
	shares: Share[];
}

export interface ViewCap {
	cap: string;
	expires_at: string;
}

export interface ShareMeta {
	project: string;
	slug: string;
	title: string;
	description: string;
	rev: number;
	updated_at: string;
	expires_at: string;
}

export interface Asset {
	name: string;
	content_type: string;
	size: number;
	sha256: string;
	created_at: string;
}

export interface ValidationProblem {
	line?: number;
	message: string;
}

export class ApiError extends Error {
	readonly status: number;

	constructor(status: number, message: string) {
		super(message);
		this.name = "ApiError";
		this.status = status;
	}
}

/** Sends the browser to the server's login endpoint; the server sets the session cookie. */
function signIn(): void {
	const here = `${location.pathname}${location.search}${location.hash}`;
	location.assign(`/api/login?next=${encodeURIComponent(here)}`);
}

export function reportKey(report: { project: string; slug: string }): string {
	return `${report.project}/${report.slug}`;
}

async function errorMessage(res: Response): Promise<string> {
	try {
		const body: unknown = await res.json();
		if (body && typeof body === "object") {
			const b = body as { error?: unknown; problems?: ValidationProblem[] };
			if (Array.isArray(b.problems) && b.problems.length > 0) {
				const problems = b.problems
					.slice(0, 3)
					.map(p => (p.line ? `line ${p.line}: ${p.message}` : p.message))
					.join("; ");
				return problems;
			}
			if (typeof b.error === "string") {
				if (b.error === "not_found") return "Not found";
				if (b.error === "unauthorized") return "Unauthorized";
				if (b.error === "rev_mismatch") return "The report changed in the meantime";
				return b.error;
			}
		}
	} catch {
		// fall through to status text
	}
	return `${res.status} ${res.statusText || "request failed"}`;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
	const res = await fetch(path, {
		credentials: "same-origin",
		...init,
		headers: { accept: "application/json", ...init?.headers },
	});
	if (res.status === 401) {
		// The server's login endpoint answers with a Basic challenge and sets the session cookie.
		signIn();
		throw new ApiError(401, "Unauthorized — signing in");
	}
	if (!res.ok) throw new ApiError(res.status, await errorMessage(res));
	if (res.status === 204) return undefined as T;
	return (await res.json()) as T;
}

async function requestText(path: string, init?: RequestInit): Promise<string> {
	const res = await fetch(path, { credentials: "same-origin", ...init });
	if (res.status === 401) {
		signIn();
		throw new ApiError(401, "Unauthorized — signing in");
	}
	if (!res.ok) throw new ApiError(res.status, await errorMessage(res));
	return await res.text();
}

const json = (body: unknown): RequestInit => ({
	method: "POST",
	headers: { "content-type": "application/json" },
	body: JSON.stringify(body),
});

export function getProjects(): Promise<Project[]> {
	return request<{ projects: Project[] }>("/api/projects").then(r => r.projects);
}

export function listReports(project?: string): Promise<ReportSummary[]> {
	const query = project ? `?project=${encodeURIComponent(project)}` : "";
	return request<{ reports: ReportSummary[] }>(`/api/reports${query}`).then(r => r.reports);
}

export function getReport(project: string, slug: string): Promise<ReportDetail> {
	return request<ReportDetail>(`/api/reports/${project}/${slug}`);
}

export function getBody(project: string, slug: string): Promise<string> {
	return requestText(`/api/reports/${project}/${slug}/body`);
}

export function mintViewCap(project: string, slug: string): Promise<ViewCap> {
	return request<ViewCap>(`/api/reports/${project}/${slug}/view-cap`, json({}));
}

export function listShares(project: string, slug: string): Promise<Share[]> {
	return request<{ shares: Share[] }>(`/api/reports/${project}/${slug}/shares`).then(r => r.shares);
}

export function createShare(project: string, slug: string): Promise<Share> {
	return request<Share>(`/api/reports/${project}/${slug}/shares`, json({}));
}

export function revokeShare(token: string): Promise<void> {
	return request<void>(`/api/shares/${encodeURIComponent(token)}`, { method: "DELETE" });
}

/** Removes the report together with its assets and share links. */
export function deleteReport(project: string, slug: string): Promise<void> {
	return request<void>(`/api/reports/${project}/${slug}`, { method: "DELETE" });
}

export function listAssets(project: string, slug: string): Promise<Asset[]> {
	return request<{ assets: Asset[] }>(`/api/reports/${project}/${slug}/assets`).then(r => r.assets);
}

export function getShareMeta(token: string): Promise<ShareMeta> {
	return request<ShareMeta>(`/api/s/${encodeURIComponent(token)}/meta`);
}

export function getShareBody(token: string): Promise<string> {
	return requestText(`/api/s/${encodeURIComponent(token)}/body`);
}
