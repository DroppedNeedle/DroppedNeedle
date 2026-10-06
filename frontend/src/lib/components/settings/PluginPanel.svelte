<script lang="ts">
	import { onMount } from 'svelte';

	import { api } from '$lib/api/client';
	import { PLUGIN_ENDPOINTS } from '$lib/queries/plugins/endpoints';
	import { fetchPluginSources } from '$lib/queries/plugins/PluginSourceQueries.svelte';

	interface Props {
		pluginName: string;
		displayName?: string;
	}

	let { pluginName, displayName = pluginName }: Props = $props();

	let frame: HTMLIFrameElement | null = $state(null);
	// `csp` is valid on <iframe> in browsers but absent from Svelte's HTMLProps —
	// set it imperatively so svelte-check stays green. Defense-in-depth alongside
	// the srcdoc <meta> CSP below: the bundle cannot fetch() out.
	$effect(() => {
		frame?.setAttribute('csp', "default-src 'none'; connect-src 'none'; form-action 'none'");
	});
	let srcdoc: string = $state('');
	let loading: boolean = $state(true);
	let loadError: string | null = $state(null);

	const RPC_METHODS = new Set(['sources.list', 'search.preview', 'get_settings', 'health.get']);

	function escapeForInlineScript(js: string): string {
		return js.replace(/<[/]script/gi, '<\\' + '/script');
	}

	function buildSrcdoc(panelJs: string): string {
		const inline = escapeForInlineScript(panelJs);
		return (
			`<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none';"><style>body{font:13px/1.5 system-ui,sans-serif;margin:0;padding:12px;color:#111;background:#fff}#panel-root{min-height:40px}</style></head><body><div id="panel-root"></div><script>${inline}<` +
			`/script></body></html>`
		);
	}

	function reply(target: Window | null, id: unknown, payload: Record<string, unknown>): void {
		try {
			target?.postMessage({ id, ...payload }, '*');
		} catch {
			// opaque-origin target may reject; panel stays usable without the reply
		}
	}

	async function handleRpc(
		method: string,
		params: Record<string, unknown>,
		signal: AbortSignal
	): Promise<unknown> {
		if (method === 'sources.list') {
			return fetchPluginSources(signal);
		}
		if (method === 'search.preview') {
			const query = typeof params.query === 'string' ? params.query.slice(0, 200) : '';
			return api.global.get<unknown>(
				PLUGIN_ENDPOINTS.ext(pluginName, 'search', query ? { query } : undefined),
				{ signal }
			);
		}
		if (method === 'get_settings') {
			const list = await api.global.v3.GET(PLUGIN_ENDPOINTS.list(), { signal });
			const own = list.plugins.find((p) => p.name === pluginName);
			return { settings_values: own?.settings_values ?? {} };
		}
		const list = await api.global.v3.GET(PLUGIN_ENDPOINTS.list(), { signal });
		const own = list.plugins.find((p) => p.name === pluginName);
		if (!own) throw new Error('Plugin not found');
		return { enabled: own.enabled, error: own.error, active_capabilities: own.active_capabilities };
	}

	onMount(() => {
		const controller = new AbortController();
		const { signal } = controller;
		let cancelled = false;

		async function load(): Promise<void> {
			try {
				const res = await api.global.get<Response>(PLUGIN_ENDPOINTS.uiBundle(pluginName), {
					signal,
					raw: true
				});
				if (!res.ok) throw new Error(`Panel unavailable (${res.status})`);
				const js = await res.text();
				if (cancelled) return;
				if (!js.trim()) throw new Error('Panel is empty');
				srcdoc = buildSrcdoc(js);
			} catch (error) {
				if (cancelled || signal.aborted) return;
				loadError = error instanceof Error ? error.message : 'Panel failed to load';
			} finally {
				if (!cancelled) loading = false;
			}
		}

		function onMessage(event: MessageEvent): void {
			if (event.origin !== 'null') return;
			if (!frame || event.source !== frame.contentWindow) return;
			const data = event.data as { id?: unknown; method?: unknown; params?: unknown } | null;
			if (!data || typeof data !== 'object') return;
			const { id, method } = data;
			if ((typeof id !== 'string' && typeof id !== 'number') || typeof method !== 'string') return;
			if (!RPC_METHODS.has(method)) {
				reply(event.source as Window, id, {
					error: { code: 'unknown_method', message: `Unknown method ${method}` }
				});
				return;
			}
			const params =
				data.params && typeof data.params === 'object'
					? (data.params as Record<string, unknown>)
					: {};
			handleRpc(method, params, signal).then(
				(result) => reply(event.source as Window, id, { result }),
				(error: unknown) =>
					reply(event.source as Window, id, {
						error: {
							code: method === 'search.preview' ? 'search_unavailable' : 'request_failed',
							message: error instanceof Error ? error.message : 'Request failed'
						}
					})
			);
		}

		window.addEventListener('message', onMessage);
		void load();
		return () => {
			cancelled = true;
			controller.abort();
			window.removeEventListener('message', onMessage);
		};
	});
</script>

<div class="rounded-xl border border-base-content/10 bg-base-100 p-3">
	<p class="mb-2 text-xs font-semibold text-base-content/60">{displayName} panel</p>
	{#if loading}
		<div class="skeleton h-24 w-full rounded-lg"></div>
	{:else if loadError}
		<p class="text-xs text-base-content/50">Panel unavailable: {loadError}</p>
	{:else}
		<iframe
			bind:this={frame}
			title="{displayName} panel"
			sandbox="allow-scripts"
			{srcdoc}
			class="h-48 w-full rounded-lg border border-base-content/10 bg-white"
		></iframe>
		<p class="mt-1 text-[11px] text-base-content/40">Sandboxed: scripts only, no network.</p>
	{/if}
</div>
