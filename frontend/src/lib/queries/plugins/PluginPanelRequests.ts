import { api } from '$lib/api/client';
import { fetchPluginSources } from './PluginSourceQueries.svelte';
import { PLUGIN_ENDPOINTS } from './endpoints';

/** The methods a plugin's settings panel may ask its parent page for. */
export const PANEL_METHODS = new Set([
	'sources.list',
	'search.preview',
	'get_settings',
	'health.get'
]);

/**
 * Answers one request from a plugin's panel frame. The frame has no
 * requests of its own, so the page asks the server on its behalf, for that
 * plugin only, and stops when the panel closes.
 */
export async function answerPanelRequest(
	pluginName: string,
	method: string,
	params: Record<string, unknown>,
	signal: AbortSignal
): Promise<unknown> {
	if (method === 'sources.list') {
		return fetchPluginSources(signal);
	}
	if (method === 'search.preview') {
		const query = typeof params.query === 'string' ? params.query.slice(0, 200) : '';
		return api.v3.GET(PLUGIN_ENDPOINTS.ext(pluginName, 'search', query ? { query } : undefined), {
			signal
		});
	}
	const list = await api.v3.GET(PLUGIN_ENDPOINTS.list(), { signal });
	const own = list.plugins.find((p) => p.name === pluginName);
	if (method === 'get_settings') {
		return { settings_values: own?.settings_values ?? {} };
	}
	if (!own) throw new Error('Plugin not found');
	return { enabled: own.enabled, error: own.error, active_capabilities: own.active_capabilities };
}

/** Asks whether the panel page is there, since a frame cannot report a failed load. */
export async function probePanelPage(pluginName: string, signal: AbortSignal): Promise<string> {
	const address = PLUGIN_ENDPOINTS.panelPage(pluginName);
	const res = await api.head(address, { signal });
	if (res.status === 404) throw new Error('the panel file is missing or empty');
	if (!res.ok) throw new Error(`the server answered ${res.status}`);
	return address;
}
