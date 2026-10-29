import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import type { components } from '$lib/api/v3/openapi';
import { toastStore } from '$lib/stores/toast';

import { PLUGIN_ENDPOINTS } from './endpoints';
import { createAppMutation, pluginsChanged } from '$lib/queries/effects';

export type PluginInstallPreview = components['schemas']['PluginInstallPreview'];
export type PluginInstallRequest = components['schemas']['PluginInstallRequest'];

/**
 * What to say when GitHub's request budget turned a preview, install or
 * update down: the wait from `details.retry_after` (the `Retry-After`
 * seconds), rounded up to minutes. Null for any other error.
 */
export function githubWaitMessage(error: unknown): string | null {
	if (!(error instanceof ApiError) || error.code !== 'UPSTREAM_RATE_LIMITED') return null;
	const details = error.details;
	const seconds =
		details && typeof details === 'object'
			? (details as Record<string, unknown>).retry_after
			: null;
	if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds <= 0) {
		return 'GitHub requests are paced. Try again in a few minutes.';
	}
	const minutes = Math.max(1, Math.ceil(seconds / 60));
	return `GitHub requests are paced. Try again in ${minutes} ${minutes === 1 ? 'minute' : 'minutes'}.`;
}

/** Download and check a repository without installing it. */
export const previewPluginInstallMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (request: PluginInstallRequest) =>
			api.v3.POST(PLUGIN_ENDPOINTS.installPreview(), request),
		onError: (error: Error) => {
			// The install screen shows the wait itself.
			if (githubWaitMessage(error)) return;
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
			if (githubWaitMessage(error)) return;
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
			toastStore.show({
				message: githubWaitMessage(error) ?? (error.message || 'Update failed.'),
				type: 'error'
			});
		}
	}));
