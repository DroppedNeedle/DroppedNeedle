<script lang="ts">
	import { Blocks, CircleAlert, ExternalLink, RefreshCw, Trash2 } from 'lucide-svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import { getPluginsQuery } from '$lib/queries/plugins/PluginQueries.svelte';
	import {
		uninstallPluginMutation,
		updatePluginMutation
	} from '$lib/queries/plugins/PluginMutations.svelte';
	import { updatePluginFromSourceMutation } from '$lib/queries/plugins/PluginInstallMutations.svelte';
	import type { PluginInfo } from '$lib/queries/plugins/types';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import PluginHealthBadge from './PluginHealthBadge.svelte';
	import PluginInstall from './PluginInstall.svelte';
	import PluginLibraryFiles from './PluginLibraryFiles.svelte';
	import PluginPanel from './PluginPanel.svelte';

	const pluginsQuery = getPluginsQuery();
	const update = updatePluginMutation();
	const uninstall = uninstallPluginMutation();
	const sourceUpdate = updatePluginFromSourceMutation();
	const plugins = $derived(pluginsQuery.data?.plugins ?? []);

	let confirmingRemoval = $state<string | null>(null);
	// Library file lists load only once their section is opened.
	let filesOpen = $state<Record<string, boolean>>({});

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

<Card>
	{#snippet header()}
		<div class="flex items-center gap-2">
			<Blocks class="h-5 w-5 text-primary" aria-hidden="true" />
			<CardTitle>Plugins</CardTitle>
		</div>
	{/snippet}
	<p class="text-sm text-base-content/60">
		Plugins add download sources, scrobble webhooks, streaming and more. Each one runs as its own
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
					<Card variant="bordered" padding="md">
						<div>
							<div class="flex flex-wrap items-start justify-between gap-3">
								<div class="min-w-0">
									<div class="flex items-center gap-2">
										<h3 class="font-semibold">{plugin.display_name}</h3>
										<span class="text-xs text-base-content/40">v{plugin.version}</span>
										{#if outsideLink(plugin.homepage)}
											<a
												href={outsideLink(plugin.homepage)}
												target="_blank"
												rel="noopener noreferrer"
												class="text-base-content/40 hover:text-primary"
												aria-label="Plugin homepage"
											>
												<ExternalLink class="h-3.5 w-3.5" aria-hidden="true" />
											</a>
										{/if}
										{#if outsideLink(plugin.ui_external_url)}
											<a
												href={outsideLink(plugin.ui_external_url)}
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
										{#each plugin.capabilities ?? [] as capability (capability)}
											<span
												class="badge badge-sm {(plugin.active_capabilities ?? []).includes(
													capability
												)
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
										{#if plugin.capabilities?.includes('scheduler')}
											<span class="badge badge-sm badge-warning badge-outline">scheduler</span>
										{/if}
										{#if plugin.capabilities?.includes('streaming_source')}
											<span class="badge badge-sm badge-accent badge-outline">streaming</span>
										{/if}
										{#if plugin.capabilities?.includes('metadata_provider')}
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
								{#if plugin.runtime.state === 'running'}
									{@const health = plugin.runtime.health}
									<div class="mt-1.5 flex flex-wrap items-center gap-1.5 text-xs">
										<PluginHealthBadge health={health?.status ?? 'unknown'} />
										{#if health && !health.configured}
											<span class="badge badge-ghost badge-sm">Not configured</span>
										{/if}
										{#if health?.message}
											<span class="min-w-0 break-words text-base-content/60">{health.message}</span>
										{/if}
									</div>
								{/if}
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

							{#if plugin.settings_fields?.length}
								<div class="mt-3 grid gap-3 sm:grid-cols-2">
									{#each plugin.settings_fields ?? [] as field (field.key)}
										{#if field.secret}
											<SecretField
												label={field.label}
												mask={SECRET_MASK}
												help={field.help ?? undefined}
												bind:value={draft.settings[field.key]}
											/>
										{:else}
											<TextField
												label={field.label}
												help={field.help ?? undefined}
												bind:value={draft.settings[field.key]}
											/>
										{/if}
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

							{#if authStore.isAdmin && plugin.capabilities?.includes('library_writer')}
								<details
									class="mt-3 rounded-xl border border-base-content/10 p-3"
									ontoggle={(event) => (filesOpen[plugin.name] = event.currentTarget.open)}
								>
									<summary class="cursor-pointer text-xs font-semibold text-base-content/65">
										Library files
									</summary>
									{#if filesOpen[plugin.name]}
										<div class="mt-2">
											<PluginLibraryFiles pluginName={plugin.name} />
										</div>
									{/if}
								</details>
							{/if}

							<div class="mt-3 flex items-center justify-end gap-2">
								{#if confirmingRemoval === plugin.name}
									<span class="text-xs text-base-content/60">Remove this plugin?</span>
									<Button
										variant="ghost"
										size="xs"
										onclick={() => (confirmingRemoval = null)}
										disabled={uninstall.isPending}
									>
										Cancel
									</Button>
									<Button
										variant="danger"
										size="xs"
										onclick={() => {
											uninstall.mutate(plugin.name);
											confirmingRemoval = null;
										}}
										disabled={uninstall.isPending}
									>
										Remove
									</Button>
								{:else}
									<IconButton
										label="Remove {plugin.display_name}"
										icon={Trash2}
										variant="danger-ghost"
										size="xs"
										shape="square"
										onclick={() => (confirmingRemoval = plugin.name)}
									/>
									{#if plugin.install}
										<Button
											variant="ghost"
											size="xs"
											icon={RefreshCw}
											aria-label="Update {plugin.display_name} from GitHub"
											onclick={() => sourceUpdate.mutate(plugin.name)}
											disabled={sourceUpdate.isPending}
										>
											Update
										</Button>
									{/if}
									<Button
										variant="primary"
										size="xs"
										onclick={() => save(plugin)}
										disabled={update.isPending}
									>
										{update.isPending ? 'Saving…' : 'Save'}
									</Button>
								{/if}
							</div>
						</div>
					</Card>
				{/if}
			{/each}
		</div>
	{/if}
</Card>
