import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';

import {
	createAppMutation,
	libraryChanged,
	pluginFilesChanged,
	pluginsChanged
} from '$lib/queries/effects';

export const createUpdatePluginMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({
			name,
			enabled,
			settings
		}: {
			name: string;
			enabled: boolean;
			settings: Record<string, string>;
		}) => api.v3.PUT(v3('/api/v3/plugins/{name}', { path: { name } }), { enabled, settings }),
		onSuccess: (plugin) => {
			toastStore.show({
				message: plugin.error
					? `Saved ${plugin.display_name}, but it failed to load. See the error below.`
					: `${plugin.display_name} saved.`,
				type: plugin.error ? 'error' : 'success'
			});
		},
		effects: () => [pluginsChanged()],
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Saving the plugin failed.', type: 'error' });
		}
	}));

export const createUninstallPluginMutation = () =>
	createAppMutation(() => ({
		mutationFn: (name: string) => api.v3.DELETE(v3('/api/v3/plugins/{name}', { path: { name } })),
		onSuccess: () => {
			toastStore.show({ message: 'Plugin removed.', type: 'info' });
		},
		effects: () => [pluginsChanged()],
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Removing the plugin failed.', type: 'error' });
		}
	}));

// Takes back one file a plugin added. A refusal (the file changed since, its
// folder takes no changes now) is shown by the confirm dialog that awaits it.
export const createUndoPluginFileMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ bundleId }: { name: string; bundleId: string }) =>
			api.v3.POST(v3('/api/v3/library/manage/undo'), { bundle_id: bundleId }),
		onSuccess: () => {
			toastStore.show({ message: 'File removed.', type: 'success' });
		},
		effects: (_answer, { name }) => [pluginFilesChanged(name), libraryChanged()]
	}));
