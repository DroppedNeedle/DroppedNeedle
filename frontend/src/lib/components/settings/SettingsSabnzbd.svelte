<script lang="ts">
	import { FolderTree, Rss, TriangleAlert } from 'lucide-svelte';

	import {
		getSabnzbdConfigQuery,
		getSabnzbdStatusQuery,
		saveSabnzbdConfig,
		testSabnzbd
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { getIndexersQuery } from '$lib/queries/downloads/IndexerQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type {
		SabnzbdConnectionSettings,
		SabnzbdTestResult
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const statusQuery = getSabnzbdStatusQuery();
	const indexersQuery = getIndexersQuery();
	const test = testSabnzbd();

	// SABnzbd only downloads what an indexer finds - a Usenet source with no indexer is inert.
	const hasIndexer = $derived((indexersQuery.data?.length ?? 0) > 0);

	let testResult = $state<SabnzbdTestResult | null>(null);

	const section = createSettingsSection(getSabnzbdConfigQuery(), saveSabnzbdConfig(), {
		normalize: (draft) => ({
			...draft,
			category: draft.category || '*',
			downloads_mount: draft.downloads_mount || '/sabnzbd-downloads'
		}),
		test: async (draft) => {
			testResult = null;
			if (!draft.url.trim()) return { passed: false, message: 'Add the URL first.' };
			testResult = await test.mutateAsync(draft);
			return {
				passed: testResult.valid,
				message: testResult.valid
					? `Connected${testResult.version ? ` · v${testResult.version}` : ''}`
					: testResult.message
			};
		}
	});

	function statusText(enabled: boolean, url: string): string {
		const live = testResult ?? statusQuery.data;
		if (live?.valid === true) return `Connected${live.version ? ` · v${live.version}` : ''}`;
		if (!testResult && statusQuery.data) return statusQuery.data.message;
		if (!enabled) return 'Disabled';
		return url ? 'Run Test to check the connection' : 'Not configured';
	}

	// The enable switch sits in the collapsed header, so it saves at once rather
	// than waiting for Save. It is switched back if the save fails.
	async function onToggle(draft: SabnzbdConnectionSettings) {
		await section.save();
		if (section.status?.tone === 'success') {
			toastStore.show({
				message: `SABnzbd ${draft.enabled ? 'enabled' : 'disabled'}`,
				type: 'success'
			});
		} else {
			draft.enabled = !draft.enabled;
			toastStore.show({ message: 'Could not update SABnzbd', type: 'error' });
		}
	}

	function categoryOptions(category: string) {
		const names = testResult?.categories?.length
			? testResult.categories
			: [...new Set([category, '*'].filter(Boolean))];
		return names.map((name) => ({ value: name, label: name }));
	}
</script>

<QueryBoundary query={section} what="the SABnzbd settings">
	{#snippet loading()}
		<div class="skeleton h-28 w-full rounded-box"></div>
	{/snippet}
	{#snippet children(draft)}
		<DownloadClientCard
			title="SABnzbd"
			sourceLabel="Usenet"
			icon={Rss}
			connected={(testResult ?? statusQuery.data)?.valid === true}
			statusText={statusText(draft.enabled, draft.url)}
			bind:enabled={draft.enabled}
			onToggle={() => void onToggle(draft)}
			enableAriaLabel="Enable SABnzbd download client"
		>
			{#if draft.enabled && !indexersQuery.isLoading && !hasIndexer}
				<div class="alert alert-warning items-start text-sm">
					<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
					<div class="space-y-1">
						<p>
							<span class="font-semibold">No indexers configured.</span> SABnzbd downloads the NZBs your
							indexers find - with none set up, Usenet search returns nothing and this client stays idle.
						</p>
						<a class="link link-warning font-medium" href={withBasePath('/settings?tab=indexers')}>
							Add an indexer →
						</a>
					</div>
				</div>
			{/if}

			<section class="space-y-3">
				<TextField
					label="SABnzbd URL"
					mono
					bind:value={draft.url}
					placeholder="http://sabnzbd:8080"
				/>
				<SecretField
					label="API key (full)"
					mask={SECRET_MASK}
					bind:value={draft.api_key}
					placeholder="SABnzbd full API key"
				/>
			</section>

			<p class="text-xs leading-relaxed text-base-content/60">
				SABnzbd downloads the NZBs your indexers find. Use the <strong>full</strong> API key (Config →
				General), not the NZB key - the NZB key can't manage the queue.
			</p>

			<SelectField
				label="Category"
				bind:value={draft.category}
				options={categoryOptions(draft.category)}
				help="Run Test to load categories. A dedicated droppedneedle category gives predictable folders, but * works."
			/>

			<section class="space-y-2">
				<div class="flex items-center gap-2 text-sm font-semibold">
					<FolderTree class="size-4 text-base-content/70" aria-hidden="true" /> Downloads mount
				</div>
				<Card variant="bordered" padding="sm">
					<div class="space-y-1.5">
						<label class="text-sm font-medium" for="sab-mount">Downloads mount</label>
						<p class="text-xs text-base-content/60">
							Where DroppedNeedle sees SABnzbd's completed folder (its <code>complete</code> dir),
							mounted read-write into this container on the same disk as your library.
							{#if testResult?.complete_dir}
								SABnzbd's complete dir is <code class="text-base-content/70"
									>{testResult.complete_dir}</code
								>.
							{/if}
						</p>
						<Input
							id="sab-mount"
							label="Downloads mount"
							size="sm"
							mono
							class="w-full"
							bind:value={draft.downloads_mount}
							placeholder="/sabnzbd-downloads"
						/>
						{#if testResult?.mount_message}
							<p class="text-xs leading-relaxed text-warning">{testResult.mount_message}</p>
						{/if}
					</div>
				</Card>
			</section>

			<SaveBar {...section.bar} testLabel="Test connection" />
		</DownloadClientCard>
	{/snippet}
</QueryBoundary>
