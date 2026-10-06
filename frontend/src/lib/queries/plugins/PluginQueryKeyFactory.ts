import { userIdSegment } from '$lib/queries/userKeySegment';

// Plugin list/config is global admin state, not user-dependent, so no userId segment.
// Sources feed per-user acquisition surfaces (priority/review labels), so
// sources()/ui() carry userId like the downloads keys.
export const PluginQueryKeyFactory = {
	prefix: ['plugins'] as const,
	list: () => [...PluginQueryKeyFactory.prefix, 'list'] as const,
	sources: (userId?: string) =>
		[...PluginQueryKeyFactory.prefix, 'sources', userIdSegment(userId)] as const,
	ui: (userId: string | null | undefined, name: string) =>
		[...PluginQueryKeyFactory.prefix, 'ui', userIdSegment(userId), name] as const
};
