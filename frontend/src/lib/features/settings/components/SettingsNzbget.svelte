<script lang="ts">
	import { FolderTree, Rss, TriangleAlert } from 'lucide-svelte';

	import {
		getNzbgetConfigQuery,
		getNzbgetStatusQuery,
		getSabnzbdConfigQuery,
		createSaveNzbgetConfigMutation,
		createTestNzbgetMutation
	} from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';
	import { getIndexersQuery } from '$lib/features/downloads/queries/IndexerQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type {
		NzbgetConnectionSettings,
		UsenetClientTestResult
	} from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';
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

	const statusQuery = getNzbgetStatusQuery();
	const indexersQuery = getIndexersQuery();
	const sabnzbdQuery = getSabnzbdConfigQuery();
	const test = createTestNzbgetMutation();

	// NZBGet only downloads what an indexer finds - a Usenet source with no indexer is inert.
	const hasIndexer = $derived((indexersQuery.data?.length ?? 0) > 0);
	// New jobs go to SABnzbd when both are on; each client finishes the jobs it took.
	const sabnzbdOn = $derived(
		sabnzbdQuery.data?.enabled === true && Boolean(sabnzbdQuery.data?.url)
	);

	let testResult = $state<UsenetClientTestResult | null>(null);

	const section = createSettingsSection(getNzbgetConfigQuery(), createSaveNzbgetConfigMutation(), {
		normalize: (draft) => ({
			...draft,
			downloads_mount: draft.downloads_mount || '/nzbget-downloads'
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
	async function onToggle(draft: NzbgetConnectionSettings) {
		await section.save();
		if (section.status?.tone === 'success') {
			toastStore.show({
				message: `NZBGet ${draft.enabled ? 'enabled' : 'disabled'}`,
				type: 'success'
			});
		} else {
			draft.enabled = !draft.enabled;
			toastStore.show({ message: 'Could not update NZBGet', type: 'error' });
		}
	}

	function categoryOptions(category: string) {
		const names = testResult?.categories?.length
			? testResult.categories
			: [category].filter(Boolean);
		return [
			{ value: '', label: 'None' },
			...[...new Set(names)].map((name) => ({ value: name, label: name }))
		];
	}
</script>

<QueryBoundary query={section} what="the NZBGet settings">
	{#snippet loading()}
		<div class="skeleton h-28 w-full rounded-box"></div>
	{/snippet}
	{#snippet children(draft)}
		<DownloadClientCard
			title="NZBGet"
			sourceLabel="Usenet"
			icon={Rss}
			connected={(testResult ?? statusQuery.data)?.valid === true}
			statusText={statusText(draft.enabled, draft.url)}
			bind:enabled={draft.enabled}
			onToggle={() => void onToggle(draft)}
			enableAriaLabel="Enable NZBGet download client"
		>
			{#if draft.enabled && sabnzbdOn}
				<div class="alert alert-warning items-start text-sm">
					<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
					<p>
						<span class="font-semibold">SABnzbd is also on.</span> New downloads go to SABnzbd. NZBGet
						finishes the ones it already has.
					</p>
				</div>
			{/if}
			{#if draft.enabled && !indexersQuery.isLoading && !hasIndexer}
				<div class="alert alert-warning items-start text-sm">
					<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
					<div class="space-y-1">
						<p>
							<span class="font-semibold">No indexers configured.</span> NZBGet downloads the NZBs your
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
					label="NZBGet URL"
					mono
					bind:value={draft.url}
					placeholder="http://nzbget:6789"
					error={section.fieldError('url', 'NZBGet URL')}
				/>
				<TextField
					label="Username"
					bind:value={draft.username}
					placeholder="nzbget"
					error={section.fieldError('username', 'Username')}
				/>
				<SecretField
					label="Password"
					mask={SECRET_MASK}
					bind:value={draft.password}
					placeholder="NZBGet control password"
					error={section.fieldError('password', 'Password')}
				/>
			</section>

			<p class="text-xs leading-relaxed text-muted">
				Use NZBGet's control username and password (Settings → Security), the same sign-in as its
				web UI.
			</p>

			<SelectField
				label="Category"
				bind:value={draft.category}
				options={categoryOptions(draft.category)}
				help="Run Test to load NZBGet's categories. A dedicated category gives predictable folders, but none works."
				error={section.fieldError('category', 'Category')}
			/>

			<section class="space-y-2">
				<div class="flex items-center gap-2 text-sm font-semibold">
					<FolderTree class="size-4 text-muted" aria-hidden="true" /> Downloads mount
				</div>
				<Card variant="bordered" padding="sm">
					<div class="space-y-1.5">
						<label class="text-sm font-medium" for="nzbget-mount">Downloads mount</label>
						<p class="text-xs text-muted">
							Where DroppedNeedle sees NZBGet's completed folder (its <code>DestDir</code>), mounted
							read-write into this container on the same disk as your library.
							{#if testResult?.complete_dir}
								NZBGet's DestDir is <code class="text-muted">{testResult.complete_dir}</code>.
							{/if}
						</p>
						<Input
							id="nzbget-mount"
							label="Downloads mount"
							size="sm"
							mono
							class="w-full"
							bind:value={draft.downloads_mount}
							placeholder="/nzbget-downloads"
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
