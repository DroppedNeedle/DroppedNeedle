<script lang="ts">
	import { CircleCheck, CircleX, FolderTree, Rss, TriangleAlert } from 'lucide-svelte';

	import {
		getSabnzbdConfigQuery,
		getSabnzbdStatusQuery,
		saveSabnzbdConfig,
		testSabnzbd
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { getIndexersQuery } from '$lib/queries/downloads/IndexerQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { SabnzbdConnectionSettings, SabnzbdTestResult } from '$lib/types';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const configQuery = getSabnzbdConfigQuery();
	const statusQuery = getSabnzbdStatusQuery();
	const indexersQuery = getIndexersQuery();
	const save = saveSabnzbdConfig();
	const test = testSabnzbd();

	// SABnzbd only downloads what an indexer finds - a Usenet source with no indexer is inert.
	const hasIndexer = $derived((indexersQuery.data?.length ?? 0) > 0);

	let enabled = $state(false);
	let url = $state('');
	let apiKey = $state('');

	let category = $state('*');
	let downloadsMount = $state('/sabnzbd-downloads');
	let priority = $state(0);
	let postProcessing = $state(3);
	let seeded = $state(false);
	let testResult = $state<SabnzbdTestResult | null>(null);

	$effect(() => {
		const d = configQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			url = d.url;
			apiKey = d.api_key;
			category = d.category || '*';
			downloadsMount = d.downloads_mount || '/sabnzbd-downloads';
			priority = d.priority ?? 0;
			postProcessing = d.post_processing ?? 3;
			seeded = true;
		}
	});

	const connected = $derived(
		testResult ? testResult.valid === true : statusQuery.data?.valid === true
	);
	const statusText = $derived(
		testResult
			? testResult.valid === true
				? `Connected${testResult.version ? ` · v${testResult.version}` : ''}`
				: enabled
					? url
						? 'Run Test to check the connection'
						: 'Not configured'
					: 'Disabled'
			: statusQuery.data?.valid === true
				? `Connected${statusQuery.data.version ? ` · v${statusQuery.data.version}` : ''}`
				: statusQuery.data
					? statusQuery.data.message
					: enabled
						? url
							? 'Run Test to check the connection'
							: 'Not configured'
						: 'Disabled'
	);

	function current(): SabnzbdConnectionSettings {
		return {
			enabled,
			client_type: 'sabnzbd',
			url,
			api_key: apiKey,
			category,
			priority,
			post_processing: postProcessing,
			downloads_mount: downloadsMount
		};
	}

	async function onSave() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: 'SABnzbd settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save SABnzbd settings', type: 'error' });
		}
	}

	// The enable switch sits in the collapsed header, so persist it the moment it flips
	// rather than making the user expand the card and hit Save. Revert on failure.
	async function onToggle() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: `SABnzbd ${enabled ? 'enabled' : 'disabled'}`, type: 'success' });
		} catch {
			enabled = !enabled;
			toastStore.show({ message: 'Could not update SABnzbd', type: 'error' });
		}
	}

	async function onTest() {
		try {
			testResult = await test.mutateAsync(current());
		} catch {
			testResult = { valid: false, message: "Couldn't reach SABnzbd", categories: [] };
		}
	}

	const categoryOptions = $derived(
		testResult?.categories?.length
			? testResult.categories
			: [...new Set([category, '*'].filter(Boolean))]
	);
</script>

{#if configQuery.isLoading}
	<div class="skeleton h-28 w-full rounded-box"></div>
{:else if configQuery.isError}
	<div class="alert alert-error">
		Failed to load SABnzbd settings: {configQuery.error.message}
	</div>
{:else}
	<DownloadClientCard
		title="SABnzbd"
		sourceLabel="Usenet"
		icon={Rss}
		{connected}
		{statusText}
		bind:enabled
		{onToggle}
		enableAriaLabel="Enable SABnzbd download client"
	>
		{#if enabled && !indexersQuery.isLoading && !hasIndexer}
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
			<TextField label="SABnzbd URL" mono bind:value={url} placeholder="http://sabnzbd:8080" />
			<SecretField
				label="API key (full)"
				mask={configQuery.data?.api_key || undefined}
				bind:value={apiKey}
				placeholder="SABnzbd full API key"
			/>
			<div class="flex flex-wrap items-center gap-3">
				<Button
					variant="outline"
					size="sm"
					loading={test.isPending}
					disabled={!url}
					onclick={onTest}
				>
					Test connection
				</Button>
				{#if testResult}
					<span
						class="flex items-center gap-1.5 text-sm"
						class:text-success={testResult.valid}
						class:text-error={!testResult.valid}
					>
						{#if testResult.valid}
							<CircleCheck class="size-4" aria-hidden="true" /> Connected{testResult.version
								? ` · v${testResult.version}`
								: ''}
						{:else}
							<CircleX class="size-4" aria-hidden="true" /> {testResult.message}
						{/if}
					</span>
				{/if}
			</div>
		</section>

		<p class="text-xs leading-relaxed text-base-content/60">
			SABnzbd downloads the NZBs your indexers find. Use the <strong>full</strong> API key (Config → General),
			not the NZB key - the NZB key can't manage the queue.
		</p>

		<SelectField
			label="Category"
			bind:value={category}
			options={categoryOptions.map((opt) => ({ value: opt, label: opt }))}
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
						bind:value={downloadsMount}
						placeholder="/sabnzbd-downloads"
					/>
					{#if testResult?.mount_message}
						<p class="text-xs leading-relaxed text-warning">{testResult.mount_message}</p>
					{/if}
				</div>
			</Card>
		</section>

		<div class="flex justify-end">
			<Button variant="primary" loading={save.isPending} onclick={onSave}>Save settings</Button>
		</div>
	</DownloadClientCard>
{/if}
