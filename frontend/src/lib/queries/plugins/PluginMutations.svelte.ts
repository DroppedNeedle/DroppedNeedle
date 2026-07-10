import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import type { PluginInfo } from './types';

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
		}) => api.global.put<PluginInfo>(API.plugins.update(name), { enabled, settings }),
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

export const installPluginMutation = () =>
	createMutation(() => ({
		mutationFn: (repositoryUrl: string) =>
			api.global.post<PluginInfo>(API.plugins.install(), { repository_url: repositoryUrl }),
		onSuccess: async (plugin) => {
			toastStore.show({
				message: `Installed ${plugin.display_name}. Review it, then enable it below.`,
				type: 'success'
			});
			await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.prefix });
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Install failed.', type: 'error' });
		}
	}));

export const uninstallPluginMutation = () =>
	createMutation(() => ({
		mutationFn: (name: string) => api.global.delete(API.plugins.uninstall(name)),
		onSuccess: async () => {
			toastStore.show({ message: 'Plugin removed.', type: 'info' });
			await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.prefix });
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Removing the plugin failed.', type: 'error' });
		}
	}));

export const fetchFromSourceMutation = () =>
	createMutation(() => ({
		mutationFn: ({ plugin, itemId }: { plugin: string; itemId: string }) =>
			api.global.post(API.plugins.sourceFetch(plugin), { item_id: itemId }),
		onSuccess: () => {
			toastStore.show({
				message: "Fetching. It'll appear in your imports when it lands.",
				type: 'success'
			});
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'The fetch failed to start.', type: 'error' });
		}
	}));
