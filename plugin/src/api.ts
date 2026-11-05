/**
 * HTTP client and persisted settings for the agent-reports plugin.
 *
 * Settings live in the omp plugin lockfile (`omp plugin config set agent-reports …`),
 * with project-level overrides honoured through the host plugin loader. The stored
 * `session` is one string `"<origin> <cookie>"`: the server's session cookie plus the
 * origin it was issued for. A request carries `Cookie: __Host-agent-reports=<cookie>` only
 * when `url` has that origin, so a project override of `url` never receives the cookie.
 */
import { getPluginSettings } from "@oh-my-pi/pi-coding-agent/extensibility/plugins/loader";
import { PluginManager } from "@oh-my-pi/pi-coding-agent/extensibility/plugins/manager";
import {
	isProjectSummary,
	isRecord,
	isReportSummary,
	isUploadedAsset,
	type ProjectSummary,
	type ReportSummary,
	readArray,
	readErrorField,
	readValidationProblems,
	type UploadedAsset,
	type ValidationProblem,
} from "./guards";

export const PLUGIN_NAME = "agent-reports";
/** Local dev default; a deployment sets `url` with `omp plugin config set`. */
export const DEFAULT_URL = "http://localhost:8080";
export const COOKIE_NAME = "__Host-agent-reports";
/** Server limit for a report body (contract: 413 above this). */
export const MAX_SOURCE_BYTES = 5 * 1024 * 1024;
/** Server limit for a single asset. */
export const MAX_ASSET_BYTES = 25 * 1024 * 1024;
/** Project and slug grammar enforced by the server. */
export const IDENTITY_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;

export type { ProjectSummary, ReportSummary, UploadedAsset, ValidationProblem };

/** The session cookie and the origin of the `url` it was issued for. */
export interface StoredSession {
	origin: string;
	cookie: string;
}

export interface AgentReportsConfig {
	url: string;
	/** Parsed `session` setting; absent when unset or not in `"<origin> <cookie>"` form. */
	session?: StoredSession;
	/** A `session` value is set but unreadable (e.g. a bare cookie from an older plugin version). */
	staleSession: boolean;
}

export interface PutReportResult {
	report: ReportSummary;
	created: boolean;
	url: string;
}

/** A non-2xx answer from the service, or a transport-level failure. */
export class ApiError extends Error {
	readonly status: number;
	readonly body: unknown;

	constructor(status: number, message: string, body?: unknown) {
		super(message);
		this.name = "ApiError";
		this.status = status;
		this.body = body;
	}
}

interface CacheEntry<T> {
	at: number;
	value: T;
}

const configCache = new Map<string, CacheEntry<AgentReportsConfig>>();
const CONFIG_TTL_MS = 1000;

function normalizeBaseUrl(value: string): string {
	const trimmed = value.trim().replace(/\/+$/, "");
	return trimmed || DEFAULT_URL;
}

/** Origin of `url`, or undefined when it does not parse. */
export function urlOrigin(url: string): string | undefined {
	try {
		return new URL(url).origin;
	} catch {
		return undefined;
	}
}

/** Parse a stored `"<origin> <cookie>"` value; anything else is undefined. */
export function parseSession(raw: string): StoredSession | undefined {
	const match = /^(\S+) (\S+)$/.exec(raw.trim());
	const origin = match?.[1];
	const cookie = match?.[2];
	if (origin === undefined || cookie === undefined) return undefined;
	let parsed: URL;
	try {
		parsed = new URL(origin);
	} catch {
		return undefined;
	}
	if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return undefined;
	if (parsed.origin !== origin) return undefined;
	return { origin, cookie };
}

/** Serialize a session for the `session` setting. */
export function formatSession(session: StoredSession): string {
	return `${session.origin} ${session.cookie}`;
}

/** True when a session is stored and was issued for the origin of `cfg.url`. */
export function sessionMatchesUrl(cfg: AgentReportsConfig): boolean {
	return cfg.session !== undefined && cfg.session.origin === urlOrigin(cfg.url);
}

/** Read `url` + `session` for `cwd`, merging global settings and project overrides. */
export async function readConfig(cwd: string): Promise<AgentReportsConfig> {
	const cached = configCache.get(cwd);
	if (cached !== undefined && Date.now() - cached.at < CONFIG_TTL_MS) return cached.value;
	let settings: Record<string, unknown> = {};
	try {
		settings = await getPluginSettings(PLUGIN_NAME, cwd);
	} catch {
		settings = {};
	}
	const rawUrl = typeof settings.url === "string" ? settings.url.trim() : "";
	const rawSession = typeof settings.session === "string" ? settings.session.trim() : "";
	const session = rawSession === "" ? undefined : parseSession(rawSession);
	const value: AgentReportsConfig = {
		url: normalizeBaseUrl(rawUrl),
		session,
		staleSession: rawSession !== "" && session === undefined,
	};
	configCache.set(cwd, { at: Date.now(), value });
	return value;
}

/** Drop the memoized config (after login/logout). */
export function invalidateConfig(): void {
	configCache.clear();
}

/** Persist the session as `"<origin> <cookie>"`; `undefined` deletes the setting. */
export async function writeSession(cwd: string, session: StoredSession | undefined): Promise<void> {
	const manager = new PluginManager(cwd);
	if (session === undefined) await manager.deletePluginSetting(PLUGIN_NAME, "session");
	else await manager.setPluginSetting(PLUGIN_NAME, "session", formatSession(session));
	invalidateConfig();
}

async function parseBody(response: Response): Promise<unknown> {
	const text = await response.text().catch(() => "");
	if (text === "") return undefined;
	try {
		return JSON.parse(text) as unknown;
	} catch {
		return text;
	}
}

function errorMessageFrom(body: unknown, fallback: string): string {
	return readErrorField(body) ?? (typeof body === "string" && body.trim() !== "" ? body.trim() : fallback);
}

/** Why a 401 happened, phrased for the model and the status command. */
export function unauthorizedMessage(cfg: AgentReportsConfig): string {
	if (cfg.staleSession) return "stored session has an old format — run /agent-reports login again";
	if (cfg.session !== undefined && !sessionMatchesUrl(cfg)) {
		const origin = urlOrigin(cfg.url) ?? cfg.url;
		return `not logged in for ${origin}: the stored session belongs to ${cfg.session.origin}, url is ${origin} — run /agent-reports login`;
	}
	return "not logged in — run /agent-reports login";
}

/**
 * Send a request to `cfg.url + pathname`. The cookie goes only to the origin it was
 * issued for. Only 401 is thrown here; callers map other statuses.
 */
export async function request(
	cfg: AgentReportsConfig,
	pathname: string,
	init: RequestInit & { signal?: AbortSignal } = {},
): Promise<Response> {
	const headers = new Headers(init.headers);
	if (cfg.session !== undefined && sessionMatchesUrl(cfg)) {
		headers.set("Cookie", `${COOKIE_NAME}=${cfg.session.cookie}`);
	}
	let response: Response;
	try {
		response = await fetch(`${cfg.url}${pathname}`, { ...init, headers, redirect: "manual" });
	} catch (error) {
		throw new ApiError(0, `cannot reach ${cfg.url}: ${error instanceof Error ? error.message : String(error)}`);
	}
	if (response.status === 401) {
		throw new ApiError(401, unauthorizedMessage(cfg), await parseBody(response));
	}
	return response;
}

/** `GET /api/me`: true when a session for this origin is stored and accepted. */
export async function checkLogin(cfg: AgentReportsConfig): Promise<boolean> {
	if (!sessionMatchesUrl(cfg)) return false;
	try {
		const response = await request(cfg, "/api/me");
		return response.ok;
	} catch (error) {
		if (error instanceof ApiError && error.status === 401) return false;
		throw error;
	}
}

/**
 * `GET /api/login` with HTTP Basic auth, returning the session cookie the server sets.
 * Emulates the browser login: the credentials are never persisted, only the cookie.
 */
export async function loginWithBasic(
	cfg: AgentReportsConfig,
	username: string,
	password: string,
): Promise<{ ok: boolean; status: number; cookie?: string }> {
	const credentials = Buffer.from(`${username}:${password}`, "utf8").toString("base64");
	let response: Response;
	try {
		response = await fetch(`${cfg.url}/api/login`, {
			headers: { Authorization: `Basic ${credentials}` },
			redirect: "manual",
		});
	} catch (error) {
		throw new ApiError(0, `cannot reach ${cfg.url}: ${error instanceof Error ? error.message : String(error)}`);
	}
	const cookies = typeof response.headers.getSetCookie === "function" ? response.headers.getSetCookie() : [];
	const cookie = cookies
		.map(value => value.split(";")[0]?.trim() ?? "")
		.find(value => value.startsWith(`${COOKIE_NAME}=`));
	return {
		ok: response.status === 303 || response.ok,
		status: response.status,
		cookie: cookie === undefined ? undefined : cookie.slice(COOKIE_NAME.length + 1),
	};
}

export async function listProjects(cfg: AgentReportsConfig, signal?: AbortSignal): Promise<ProjectSummary[]> {
	const response = await request(cfg, "/api/projects", { signal });
	const body = await parseBody(response);
	if (!response.ok) {
		throw new ApiError(response.status, errorMessageFrom(body, `HTTP ${response.status} from /api/projects`), body);
	}
	return readArray(body, "projects", isProjectSummary);
}

export async function listReports(
	cfg: AgentReportsConfig,
	project: string | undefined,
	signal?: AbortSignal,
): Promise<ReportSummary[]> {
	const query = project === undefined ? "" : `?project=${encodeURIComponent(project)}`;
	const response = await request(cfg, `/api/reports${query}`, { signal });
	const body = await parseBody(response);
	if (!response.ok) {
		throw new ApiError(response.status, errorMessageFrom(body, `HTTP ${response.status} from /api/reports`), body);
	}
	return readArray(body, "reports", isReportSummary);
}

function revFromHeaders(response: Response): number {
	const header = response.headers.get("X-Agent-Reports-Rev") ?? "";
	if (/^\d+$/.test(header.trim())) return Number.parseInt(header.trim(), 10);
	const etag = (response.headers.get("ETag") ?? "").replace(/^W\//, "").replace(/"/g, "").trim();
	if (/^\d+$/.test(etag)) return Number.parseInt(etag, 10);
	return 0;
}

/** `GET …/source`; null when the report does not exist. */
export async function fetchSource(
	cfg: AgentReportsConfig,
	project: string,
	slug: string,
	signal?: AbortSignal,
): Promise<{ text: string; rev: number } | null> {
	const response = await request(cfg, `/api/reports/${project}/${slug}/source`, { signal, cache: "no-store" });
	if (response.status === 404) return null;
	if (!response.ok) {
		const body = await parseBody(response);
		throw new ApiError(
			response.status,
			errorMessageFrom(body, `HTTP ${response.status} reading ${project}/${slug}`),
			body,
		);
	}
	return { text: await response.text(), rev: revFromHeaders(response) };
}

/** `GET …/text`; null when the report does not exist. */
export async function fetchTextVersion(
	cfg: AgentReportsConfig,
	project: string,
	slug: string,
	signal?: AbortSignal,
): Promise<string | null> {
	const response = await request(cfg, `/api/reports/${project}/${slug}/text`, { signal, cache: "no-store" });
	if (response.status === 404) return null;
	if (!response.ok) {
		const body = await parseBody(response);
		throw new ApiError(
			response.status,
			errorMessageFrom(body, `HTTP ${response.status} reading ${project}/${slug}`),
			body,
		);
	}
	return await response.text();
}

/** `PUT …/source` with optimistic concurrency. Throws {@link ApiError} on 412/413/422. */
export async function putSource(
	cfg: AgentReportsConfig,
	project: string,
	slug: string,
	text: string,
	precondition: { ifMatch?: number; ifNoneMatch?: true },
	signal?: AbortSignal,
): Promise<PutReportResult> {
	const headers: Record<string, string> = { "Content-Type": "text/plain; charset=utf-8" };
	if (precondition.ifMatch !== undefined) headers["If-Match"] = `"${precondition.ifMatch}"`;
	else if (precondition.ifNoneMatch) headers["If-None-Match"] = "*";
	const response = await request(cfg, `/api/reports/${project}/${slug}/source`, {
		method: "PUT",
		headers,
		body: text,
		signal,
	});
	const body = await parseBody(response);
	if (!response.ok) {
		throw new ApiError(
			response.status,
			errorMessageFrom(body, `HTTP ${response.status} saving ${project}/${slug}`),
			body,
		);
	}
	if (!isRecord(body) || !isReportSummary(body.report) || typeof body.url !== "string") {
		throw new ApiError(response.status, `unexpected response saving ${project}/${slug}`, body);
	}
	return { report: body.report, created: body.created === true, url: body.url };
}

/** `PUT …/assets/:name`. */
export async function uploadAsset(
	cfg: AgentReportsConfig,
	project: string,
	slug: string,
	name: string,
	contentType: string,
	bytes: Uint8Array,
	signal?: AbortSignal,
): Promise<UploadedAsset> {
	const headers: Record<string, string> = { "Content-Type": contentType };
	// Bun's fetch accepts a Uint8Array body; lib.dom's BodyInit typing does not model it.
	const payload = bytes as unknown as BodyInit;
	const response = await request(cfg, `/api/reports/${project}/${slug}/assets/${encodeURIComponent(name)}`, {
		method: "PUT",
		headers,
		body: payload,
		signal,
	});
	const body = await parseBody(response);
	if (!response.ok) {
		throw new ApiError(response.status, errorMessageFrom(body, `HTTP ${response.status} uploading ${name}`), body);
	}
	if (!isUploadedAsset(body)) throw new ApiError(response.status, `unexpected response uploading ${name}`, body);
	return body;
}

/** Human-readable 422 problems (contract: `{error:"validation", problems:[{line?, message}]}`). */
export function validationProblems(error: ApiError): ValidationProblem[] {
	return readValidationProblems(error.body);
}

export function isRevMismatch(error: unknown): error is ApiError {
	return error instanceof ApiError && error.status === 412;
}
