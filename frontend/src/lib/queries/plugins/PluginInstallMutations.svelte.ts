import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import { PLUGIN_ENDPOINTS } from './endpoints';

export type PluginInstallPreview = components['schemas']['PluginInstallPreview'];
export type PluginInstallRequest = components['schemas']['PluginInstallRequest'];

/** Download and check a repository without installing it. */
export const previewPluginInstallMutation = () =>
	createMutation(() => ({
		mutationFn: (request: PluginInstallRequest) =>
			api.global.v3.POST(PLUGIN_ENDPOINTS.installPreview(), request),
		onError: (error: Error) => {
			toastStore.show({
				message: error.message || 'Could not read that repository.',
				type: 'error'
			});
		}
	}));

/** Install exactly the commit the admin previewed. */
export const installPreviewedPluginMutation = () =>
	createMutation(() => ({
		mutationFn: (preview: PluginInstallPreview & { repository_url: string }) =>
			api.global.v3.POST(PLUGIN_ENDPOINTS.install(), {
				repository_url: preview.repository_url,
				commit: preview.commit
			}),
		onSuccess: async (plugin) => {
			toastStore.show({
				message: `Installed ${plugin.display_name}. Enable it below when you are ready.`,
				type: 'success'
			});
			await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.prefix });
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Install failed.', type: 'error' });
		}
	}));

/** Move an installed plugin to its newest release (or the same ref). */
export const updatePluginFromSourceMutation = () =>
	createMutation(() => ({
		mutationFn: (name: string) => api.global.v3.POST(PLUGIN_ENDPOINTS.update(name), {}),
		onSuccess: async (result) => {
			toastStore.show({
				message: result.updated
					? `${result.plugin.display_name} updated to ${result.plugin.version}.`
					: `${result.plugin.display_name} is already up to date.`,
				type: result.updated ? 'success' : 'info'
			});
			await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.prefix });
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Update failed.', type: 'error' });
		}
	}));
