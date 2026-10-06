import { v3, type V3Query } from '$lib/api/v3/endpoint';

export const PLUGIN_ENDPOINTS = {
	list: () => v3('/api/v3/plugins'),
	installPreview: () => v3('/api/v3/plugins/install/preview'),
	install: () => v3('/api/v3/plugins/install'),
	sources: () => v3('/api/v3/plugins/sources'),
	plugin: (name: string) => v3('/api/v3/plugins/{name}', { path: { name } }),
	update: (name: string) => v3('/api/v3/plugins/{name}/update', { path: { name } }),
	uiBundle: (name: string) => v3('/api/v3/plugins/{name}/ui/panel.js', { path: { name } }),
	// One path segment under the plugin's extension root; the server route is
	// a wildcard, so nested subpaths are not needed by any caller.
	ext: (name: string, subpath: string, query?: V3Query) =>
		v3('/api/v3/plugins/ext/{name}/{subpath}', { path: { name, subpath }, query })
} as const;
