// Plugin list/config is global admin state; the sources list gates a curator
// surface. Neither is user-dependent, so no userId segment.
export const PluginQueryKeyFactory = {
	prefix: ['plugins'] as const,
	list: () => [...PluginQueryKeyFactory.prefix, 'list'] as const,
	sources: () => [...PluginQueryKeyFactory.prefix, 'sources'] as const,
	sourceSearch: (plugin: string, query: string) =>
		[...PluginQueryKeyFactory.prefix, 'source-search', plugin, query] as const
};
