export type Route =
	| { name: "list" }
	| { name: "report"; project: string; slug: string }
	| { name: "split"; panes: string[] }
	| { name: "notfound"; path: string };

const IDENTITY = /^[a-z0-9][a-z0-9_-]{0,63}$/;

/** Prefixed paths the SPA must not intercept (served by the backend or the static server). */
const PASSTHROUGH = ["/api/", "/_render/", "/s/", "/assets/"];

function splitPaneKeys(params: URLSearchParams): string[] {
	const keys = params.getAll("r").slice(0, 4);
	return keys.length > 0 ? keys : [""];
}

function parse(url: URL): Route {
	const parts = url.pathname.split("/").filter(Boolean);
	if (parts.length === 0) return { name: "list" };
	if (parts[0] === "r" && parts.length === 3) {
		const project = decodeURIComponent(parts[1]);
		const slug = decodeURIComponent(parts[2]);
		if (IDENTITY.test(project) && IDENTITY.test(slug)) return { name: "report", project, slug };
	}
	if (parts[0] === "split" && parts.length === 1) return { name: "split", panes: splitPaneKeys(url.searchParams) };
	return { name: "notfound", path: url.pathname };
}

class Router {
	url = $state(new URL(location.href));
	route = $derived.by(() => parse(this.url));

	navigate(to: string, options: { replace?: boolean } = {}): void {
		if (options.replace) history.replaceState(null, "", to);
		else history.pushState(null, "", to);
		this.url = new URL(location.href);
		if (!options.replace) window.scrollTo({ top: 0 });
	}
}

export const router = new Router();

export function splitUrl(panes: string[]): string {
	const params = panes.map(pane => `r=${encodeURIComponent(pane).replaceAll("%2F", "/")}`).join("&");
	return params ? `/split?${params}` : "/split";
}

window.addEventListener("popstate", () => {
	router.url = new URL(location.href);
});

document.addEventListener("click", event => {
	if (event.defaultPrevented || event.button !== 0) return;
	if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
	const target = event.target instanceof Element ? event.target.closest("a[href]") : null;
	if (!(target instanceof HTMLAnchorElement)) return;
	if (target.target || target.hasAttribute("download")) return;
	const url = new URL(target.href, location.href);
	if (url.origin !== location.origin) return;
	if (url.pathname === "/" || PASSTHROUGH.some(prefix => url.pathname.startsWith(prefix))) return;
	event.preventDefault();
	router.navigate(`${url.pathname}${url.search}`);
});
