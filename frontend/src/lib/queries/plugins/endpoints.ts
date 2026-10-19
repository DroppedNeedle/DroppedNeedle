import { v3, withRawQuery } from '$lib/api/v3/endpoint';

export const PLUGIN_ENDPOINTS = {
	list: () => v3('/api/v3/plugins'),
	installPreview: () => v3('/api/v3/plugins/install/preview'),
	install: () => v3('/api/v3/plugins/install'),
	sources: () => v3('/api/v3/plugins/sources'),
	plugin: (name: string) => v3('/api/v3/plugins/{name}', { path: { name } }),
	update: (name: string) => v3('/api/v3/plugins/{name}/update', { path: { name } }),
	panelPage: (name: string) => v3('/api/v3/plugins/{name}/ui/panel', { path: { name } }),
	// One path segment under the plugin's extension root; the server route is
	// a wildcard, so nested subpaths are not needed by any caller.
	// The server forwards the query to the plugin unchanged, so the contract lists none.
	ext: (name: string, subpath: string, query?: Record<string, string>) =>
		withRawQuery(v3('/api/v3/plugins/ext/{name}/{subpath}', { path: { name, subpath } }), query)
} as const;
