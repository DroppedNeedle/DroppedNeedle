import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { toastStore } from '$lib/stores/toast';

import { PLUGIN_ENDPOINTS } from './endpoints';
import { createAppMutation, pluginsChanged } from '$lib/queries/effects';

export type PluginInstallPreview = components['schemas']['PluginInstallPreview'];
export type PluginInstallRequest = components['schemas']['PluginInstallRequest'];

/** Download and check a repository without installing it. */
export const previewPluginInstallMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: PluginInstallRequest) =>
			api.v3.POST(PLUGIN_ENDPOINTS.installPreview(), request),
		onError: (error: Error) => {
			toastStore.show({
				message: error.message || 'Could not read that repository.',
				type: 'error'
			});
		}
	}));

/** Install exactly the commit the admin previewed. */
export const installPreviewedPluginMutation = () =>
	createAppMutation(() => ({
		mutationFn: (preview: PluginInstallPreview & { repository_url: string }) =>
			api.v3.POST(PLUGIN_ENDPOINTS.install(), {
				repository_url: preview.repository_url,
				commit: preview.commit
			}),
		onSuccess: (plugin) => {
			toastStore.show({
				message: `Installed ${plugin.display_name}. Enable it below when you are ready.`,
				type: 'success'
			});
		},
		effects: () => [pluginsChanged()],
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Install failed.', type: 'error' });
		}
	}));

/** Move an installed plugin to its newest release (or the same ref). */
export const updatePluginFromSourceMutation = () =>
	createAppMutation(() => ({
		mutationFn: (name: string) => api.v3.POST(PLUGIN_ENDPOINTS.update(name), {}),
		onSuccess: (result) => {
			toastStore.show({
				message: result.updated
					? `${result.plugin.display_name} updated to ${result.plugin.version}.`
					: `${result.plugin.display_name} is already up to date.`,
				type: result.updated ? 'success' : 'info'
			});
		},
		effects: () => [pluginsChanged()],
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Update failed.', type: 'error' });
		}
	}));
