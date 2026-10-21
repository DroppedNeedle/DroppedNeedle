import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import { PLUGIN_ENDPOINTS } from './endpoints';

export const updatePluginMutation = () =>
	createMutation(() => ({
		mutationFn: ({
			name,
			enabled,
			settings
		}: {
			name: string;
			enabled: boolean;
			settings: Record<string, string>;
		}) => api.global.v3.PUT(PLUGIN_ENDPOINTS.plugin(name), { enabled, settings }),
		onSuccess: async (plugin) => {
			toastStore.show({
				message: plugin.error
					? `Saved ${plugin.display_name}, but it failed to load. See the error below.`
					: `${plugin.display_name} saved.`,
				type: plugin.error ? 'error' : 'success'
			});
			await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.prefix });
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Saving the plugin failed.', type: 'error' });
		}
	}));

export const uninstallPluginMutation = () =>
	createMutation(() => ({
		mutationFn: (name: string) => api.global.v3.DELETE(PLUGIN_ENDPOINTS.plugin(name)),
		onSuccess: async () => {
			toastStore.show({ message: 'Plugin removed.', type: 'info' });
			await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.prefix });
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Removing the plugin failed.', type: 'error' });
		}
	}));

// Takes back one file a plugin added. A refusal (the file changed since, its
// folder takes no changes now) is shown by the confirm dialog that awaits it.
export const undoPluginFileMutation = () =>
	createMutation(() => ({
		mutationFn: ({ bundleId }: { name: string; bundleId: string }) =>
			api.global.v3.POST(PLUGIN_ENDPOINTS.undoFile(), { bundle_id: bundleId }),
		onSuccess: async (_answer, { name }) => {
			toastStore.show({ message: 'File removed.', type: 'success' });
			await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.files(name) });
		}
	}));
