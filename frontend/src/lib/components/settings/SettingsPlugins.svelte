<script lang="ts">
	import { Blocks, CircleAlert, ExternalLink, RefreshCw, Trash2 } from 'lucide-svelte';
	import { getPluginsQuery } from '$lib/queries/plugins/PluginQueries.svelte';
	import {
		uninstallPluginMutation,
		updatePluginMutation
	} from '$lib/queries/plugins/PluginMutations.svelte';
	import { updatePluginFromSourceMutation } from '$lib/queries/plugins/PluginInstallMutations.svelte';
	import type { PluginInfo } from '$lib/queries/plugins/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import PluginInstall from './PluginInstall.svelte';
	import PluginPanel from './PluginPanel.svelte';

	const pluginsQuery = getPluginsQuery();
	const update = updatePluginMutation();
	const uninstall = uninstallPluginMutation();
	const sourceUpdate = updatePluginFromSourceMutation();
	const plugins = $derived(pluginsQuery.data?.plugins ?? []);

	let confirmingRemoval = $state<string | null>(null);

	const runtimeLabels: Record<string, string> = {
		starting: 'starting',
		running: 'running',
		restarting: 'restarting',
		stopped: 'stopped',
		failed: 'failed'
	};

	// local draft per plugin so typing doesn't fight the query cache; seeded in
	// an effect (never during render - that would be a state_unsafe_mutation)
	let drafts = $state<Record<string, { enabled: boolean; settings: Record<string, string> }>>({});

	$effect(() => {
		for (const plugin of plugins) {
			if (!drafts[plugin.name]) {
				drafts[plugin.name] = {
					enabled: plugin.enabled,
					settings: { ...plugin.settings_values }
				};
			}
		}
	});

	function save(plugin: PluginInfo) {
		const draft = drafts[plugin.name];
		if (!draft) return;
		update.mutate({ name: plugin.name, enabled: draft.enabled, settings: draft.settings });
	}
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<div class="flex items-center gap-2">
			<Blocks class="h-5 w-5 text-primary" aria-hidden="true" />
			<h2 class="card-title">Plugins</h2>
		</div>
		<p class="text-sm text-base-content/60">
			Plugins add download sources, scrobble webhooks, buy links and more. Each one runs as its own
			program on your server, with the same file and network access as DroppedNeedle. It is not
			sandboxed: only install plugins you trust. See PLUGINS.md in the repository.
		</p>

		<PluginInstall />

		{#if pluginsQuery.isLoading}
			<div class="skeleton h-20 w-full rounded-xl"></div>
		{:else if plugins.length === 0}
			<p class="py-4 text-sm text-base-content/50">
				No plugins yet. Install one above, or drop a folder into <code>plugins/</code> and reload.
			</p>
		{:else}
			<div class="space-y-4 pt-2">
				{#each plugins as plugin (plugin.name)}
					{@const draft = drafts[plugin.name]}
					{#if draft}
						<div class="rounded-2xl border border-base-content/10 bg-base-100/60 p-4">
							<div class="flex flex-wrap items-start justify-between gap-3">
								<div class="min-w-0">
									<div class="flex items-center gap-2">
										<h3 class="font-semibold">{plugin.display_name}</h3>
										<span class="text-xs text-base-content/40">v{plugin.version}</span>
										{#if plugin.homepage}
											<a
												href={plugin.homepage}
												target="_blank"
												rel="noopener noreferrer"
												class="text-base-content/40 hover:text-primary"
												aria-label="Plugin homepage"
											>
												<ExternalLink class="h-3.5 w-3.5" aria-hidden="true" />
											</a>
										{/if}
										{#if plugin.ui_external_url}
											<a
												href={plugin.ui_external_url}
												target="_blank"
												rel="noopener noreferrer"
												class="text-base-content/40 hover:text-primary"
												aria-label="Plugin site"
											>
												<ExternalLink class="h-3.5 w-3.5" aria-hidden="true" />
											</a>
										{/if}
									</div>
									{#if plugin.description}
										<p class="mt-0.5 text-xs text-base-content/55">{plugin.description}</p>
									{/if}
									<div class="mt-1.5 flex flex-wrap gap-1">
										{#each plugin.capabilities as capability (capability)}
											<span
												class="badge badge-sm {plugin.active_capabilities.includes(capability)
													? 'badge-primary badge-outline'
													: 'badge-ghost'}"
											>
												{capability}
											</span>
										{/each}
										{#each plugin.sources ?? [] as source (source)}
											<span class="badge badge-sm badge-info badge-outline">src:{source}</span>
										{/each}
										{#each plugin.targets ?? [] as target (target)}
											<span class="badge badge-sm badge-success badge-outline">dst:{target}</span>
										{/each}
										{#if plugin.capabilities.includes('scheduler')}
											<span class="badge badge-sm badge-warning badge-outline">scheduler</span>
										{/if}
										{#if plugin.capabilities.includes('streaming_source')}
											<span class="badge badge-sm badge-accent badge-outline">streaming</span>
										{/if}
										{#if plugin.capabilities.includes('metadata_provider')}
											<span class="badge badge-sm badge-secondary badge-outline">metadata</span>
										{/if}
									</div>
								</div>
								<label class="flex cursor-pointer items-center gap-2">
									<span class="text-xs text-base-content/60">Enabled</span>
									<input
										type="checkbox"
										class="toggle toggle-primary toggle-sm"
										bind:checked={draft.enabled}
									/>
								</label>
							</div>

							{#if plugin.runtime}
								<p class="mt-2 text-xs text-base-content/55">
									Process: {runtimeLabels[plugin.runtime.state] ?? plugin.runtime.state}
									{#if plugin.runtime.restarts > 0}
										· restarted {plugin.runtime.restarts}×
									{/if}
									{#if (plugin.runtime.dropped_events ?? 0) > 0}
										· {plugin.runtime.dropped_events} events skipped while busy
									{/if}
								</p>
								{#if plugin.runtime.last_error && plugin.runtime.state !== 'running'}
									<div
										class="mt-1 flex items-center gap-2 rounded-lg bg-error/10 px-3 py-2 text-xs text-error"
									>
										<CircleAlert class="h-4 w-4 shrink-0" aria-hidden="true" />
										{plugin.runtime.last_error}
									</div>
								{/if}
							{/if}
							{#if plugin.install}
								<p class="mt-1 text-xs text-base-content/45">
									From {plugin.install.repository}
									{plugin.install.ref_kind === 'branch' && plugin.install.reference === 'HEAD'
										? '(default branch)'
										: `${plugin.install.ref_kind} ${plugin.install.reference}`}
									· commit <code>{plugin.install.commit.slice(0, 12)}</code>
								</p>
							{/if}

							{#if plugin.error}
								<div
									class="mt-2 flex items-center gap-2 rounded-lg bg-error/10 px-3 py-2 text-xs text-error"
								>
									<CircleAlert class="h-4 w-4 shrink-0" aria-hidden="true" />
									{plugin.error}
								</div>
							{/if}

							{#if plugin.settings_fields.length}
								<div class="mt-3 grid gap-3 sm:grid-cols-2">
									{#each plugin.settings_fields as field (field.key)}
										<div class="form-control">
											<label class="label py-1" for="plugin-{plugin.name}-{field.key}">
												<span class="label-text text-sm">{field.label}</span>
											</label>
											<input
												id="plugin-{plugin.name}-{field.key}"
												type={field.secret ? 'password' : 'text'}
												class="input input-bordered input-sm w-full"
												bind:value={draft.settings[field.key]}
											/>
											{#if field.help}
												<p class="mt-1 text-xs text-base-content/45">{field.help}</p>
											{/if}
										</div>
									{/each}
								</div>
							{/if}
							{#if authStore.isAdmin && plugin.enabled && plugin.ui_entry && !plugin.error}
								<details class="mt-3 rounded-xl border border-base-content/10 p-3">
									<summary class="cursor-pointer text-xs font-semibold text-base-content/65">
										Plugin panel
									</summary>
									<div class="mt-2">
										<PluginPanel pluginName={plugin.name} displayName={plugin.display_name} />
									</div>
								</details>
							{/if}

							<div class="mt-3 flex items-center justify-end gap-2">
								{#if confirmingRemoval === plugin.name}
									<span class="text-xs text-base-content/60">Remove this plugin?</span>
									<button
										class="btn btn-ghost btn-xs"
										onclick={() => (confirmingRemoval = null)}
										disabled={uninstall.isPending}
									>
										Cancel
									</button>
									<button
										class="btn btn-error btn-xs"
										onclick={() => {
											uninstall.mutate(plugin.name);
											confirmingRemoval = null;
										}}
										disabled={uninstall.isPending}
									>
										Remove
									</button>
								{:else}
									<button
										class="btn btn-ghost btn-xs text-base-content/50 hover:text-error"
										onclick={() => (confirmingRemoval = plugin.name)}
										aria-label="Remove {plugin.display_name}"
									>
										<Trash2 class="h-3.5 w-3.5" aria-hidden="true" />
									</button>
									{#if plugin.install}
										<button
											class="btn btn-ghost btn-xs"
											onclick={() => sourceUpdate.mutate(plugin.name)}
											disabled={sourceUpdate.isPending}
											aria-label="Update {plugin.display_name} from GitHub"
										>
											<RefreshCw class="h-3.5 w-3.5" aria-hidden="true" />
											Update
										</button>
									{/if}
									<button
										class="btn btn-primary btn-xs"
										onclick={() => save(plugin)}
										disabled={update.isPending}
									>
										{update.isPending ? 'Saving…' : 'Save'}
									</button>
								{/if}
							</div>
						</div>
					{/if}
				{/each}
			</div>
		{/if}
	</div>
</div>
