<script lang="ts">
	import { Puzzle } from 'lucide-svelte';

	import { getPluginSourcesQuery } from '$lib/queries/plugins/PluginSourceQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	import PluginHealthBadge from './PluginHealthBadge.svelte';
	import SettingsDownloadClient from './SettingsDownloadClient.svelte';
	import SettingsDownloadPolicy from './SettingsDownloadPolicy.svelte';
	import SettingsOnboardingChecklist from './SettingsOnboardingChecklist.svelte';
	import SettingsSabnzbd from './SettingsSabnzbd.svelte';
	import SettingsSourcePriority from './SettingsSourcePriority.svelte';
	import SettingsWanted from './SettingsWanted.svelte';

	const sourcesQuery = getPluginSourcesQuery();
	const pluginSources = $derived(sourcesQuery.data?.sources ?? []);
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-xl font-bold">Download clients</h2>
		<p class="text-sm text-base-content/60">
			Soulseek and Usenet are two ways to acquire music. Configure either or both, set which is
			tried first, and tune the shared policy.
		</p>
	</div>
	<SettingsDownloadClient />
	<SettingsSabnzbd />
	<Card variant="bordered">
		<div class="space-y-3">
			<div>
				<h3 class="font-semibold">Plugin sources</h3>
				<p class="text-sm text-base-content/70">
					Plugins provide extra acquisition sources. Once configured, they join the same priority
					order below.
				</p>
			</div>
			{#if sourcesQuery.isLoading}
				<div class="skeleton h-16 w-full rounded-box"></div>
			{:else if pluginSources.length === 0}
				<p class="text-sm text-base-content/50">
					No plugin sources installed.
					<a
						href={withBasePath('/settings?tab=plugins')}
						class="link link-hover font-semibold text-primary"
					>
						Configure in Plugins
					</a>
				</p>
			{:else}
				<ul class="space-y-2">
					{#each pluginSources as source (source.key)}
						<li
							class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-box border border-base-300 bg-base-100 p-2.5"
						>
							<span
								class="grid size-8 shrink-0 place-items-center rounded-lg bg-base-300/60 text-base-content/70"
							>
								<Puzzle class="size-4" aria-hidden="true" />
							</span>
							<span class="min-w-0 flex-1 basis-40">
								<span class="block truncate font-medium">{source.display_name || source.key}</span>
								<span class="block truncate text-xs text-base-content/50">{source.key}</span>
								{#if source.health_message}
									<span class="block truncate text-xs text-base-content/60">
										{source.health_message}
									</span>
								{/if}
							</span>
							<span class="flex flex-wrap items-center gap-1">
								{#if !source.configured && source.health !== 'unknown'}
									<span class="badge badge-ghost badge-sm">Not configured</span>
								{/if}
								<PluginHealthBadge health={source.health} />
							</span>
							<a
								href={withBasePath('/settings?tab=plugins')}
								class="link link-hover whitespace-nowrap text-sm font-semibold text-primary"
							>
								Configure in Plugins
							</a>
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	</Card>
	<SettingsSourcePriority />
	<SettingsDownloadPolicy />
	<SettingsWanted />
	<SettingsOnboardingChecklist />
</div>
