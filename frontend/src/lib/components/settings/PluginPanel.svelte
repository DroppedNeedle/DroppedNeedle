<script lang="ts">
	import { onMount } from 'svelte';

	import { getApiUrl } from '$lib/api/api-utils';
	import { api } from '$lib/api/client';
	import { PLUGIN_ENDPOINTS } from '$lib/queries/plugins/endpoints';
	import { fetchPluginSources } from '$lib/queries/plugins/PluginSourceQueries.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		pluginName: string;
		displayName?: string;
	}

	let { pluginName, displayName = pluginName }: Props = $props();

	// The panel is plugin code. The server serves it as a page of its own whose
	// policy header seals it, and the frame's sandbox seals it again: it gets
	// its own origin, no cookies and no requests of its own. The app's own
	// policy keeps the frame from leaving, while it is enforced. Never add
	// allow-same-origin.
	let frame: HTMLIFrameElement | null = $state(null);
	let src: string = $state('');
	let loading: boolean = $state(true);
	let loadError: string | null = $state(null);

	const RPC_METHODS = new Set(['sources.list', 'search.preview', 'get_settings', 'health.get']);

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
			return api.global.v3.GET(
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
				// A frame cannot tell the page whether its load failed, so ask first.
				const address = PLUGIN_ENDPOINTS.panelPage(pluginName);
				const res = await api.global.head(address, { signal });
				if (cancelled) return;
				if (res.status === 404) throw new Error('the panel file is missing or empty');
				if (!res.ok) throw new Error(`the server answered ${res.status}`);
				src = getApiUrl(address);
			} catch (error) {
				if (cancelled || signal.aborted) return;
				loadError = error instanceof Error ? error.message : 'it failed to load';
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

<Card variant="bordered" padding="sm">
	<div>
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
				{src}
				class="h-48 w-full rounded-lg border border-base-content/10 bg-white"
			></iframe>
			<p class="mt-1 text-[11px] text-base-content/40">
				Sandboxed: no access to this page or your session.
			</p>
		{/if}
	</div>
</Card>
