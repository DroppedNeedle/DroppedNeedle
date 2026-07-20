<script lang="ts">
	import { CircleCheck, CircleX, FolderTree, Magnet, TriangleAlert } from 'lucide-svelte';

	import {
		getProwlarrConfigQuery,
		getQbittorrentConfigQuery,
		saveQbittorrentConfig,
		testQbittorrent
	} from '$lib/queries/downloads/ProwlarrTorrentQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { QbittorrentConnectionSettings, QbittorrentTestResult } from '$lib/types';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const configQuery = getQbittorrentConfigQuery();
	const prowlarrQuery = getProwlarrConfigQuery();
	const save = saveQbittorrentConfig();
	const test = testQbittorrent();

	// The torrent source searches ONLY through Prowlarr - qBittorrent without it is inert.
	const hasProwlarr = $derived(prowlarrQuery.data?.enabled === true);

	let enabled = $state(false);
	let url = $state('');
	let apiKey = $state('');
	let showKey = $state(false);
	let category = $state('droppedneedle');
	let downloadsMount = $state('/qbittorrent-downloads');
	let seeded = $state(false);
	let testResult = $state<QbittorrentTestResult | null>(null);

	$effect(() => {
		const d = configQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			url = d.url;
			apiKey = d.api_key;
			category = d.category || 'droppedneedle';
			downloadsMount = d.downloads_mount || '/qbittorrent-downloads';
			seeded = true;
		}
	});

	const connected = $derived(testResult?.valid === true);
	const statusText = $derived(
		connected
			? `Connected${testResult?.version ? ` · v${testResult.version}` : ''}`
			: enabled
				? url
					? 'Run Test to check the connection'
					: 'Not configured'
				: 'Disabled'
	);

	function current(): QbittorrentConnectionSettings {
		return {
			enabled,
			client_type: 'qbittorrent',
			url,
			api_key: apiKey,
			category,
			downloads_mount: downloadsMount
		};
	}

	async function onSave() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: 'qBittorrent settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save qBittorrent settings', type: 'error' });
		}
	}

	// The enable switch sits in the collapsed header, so persist it the moment it flips.
	async function onToggle() {
		try {
			await save.mutateAsync(current());
			toastStore.show({
				message: `qBittorrent ${enabled ? 'enabled' : 'disabled'}`,
				type: 'success'
			});
		} catch {
			enabled = !enabled;
			toastStore.show({ message: 'Could not update qBittorrent', type: 'error' });
		}
	}

	async function onTest() {
		try {
			testResult = await test.mutateAsync(current());
		} catch {
			testResult = { valid: false, message: "Couldn't reach qBittorrent" };
		}
	}
</script>

{#if configQuery.isLoading}
	<div class="skeleton h-28 w-full rounded-box"></div>
{:else if configQuery.isError}
	<div class="alert alert-error">
		Failed to load qBittorrent settings: {configQuery.error.message}
	</div>
{:else}
	<DownloadClientCard
		title="qBittorrent"
		sourceLabel="Torrents"
		icon={Magnet}
		{connected}
		{statusText}
		bind:enabled
		{onToggle}
		enableAriaLabel="Enable qBittorrent download client"
	>
		{#if enabled && !prowlarrQuery.isLoading && !hasProwlarr}
			<div class="alert alert-warning items-start text-sm">
				<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
				<div class="space-y-1">
					<p>
						<span class="font-semibold">Prowlarr is not enabled.</span> Torrent search goes through your
						Prowlarr indexers - without it, this client stays idle.
					</p>
				</div>
			</div>
		{/if}

		<section class="space-y-3">
			<div class="form-control">
				<label class="label" for="qbt-url"><span class="label-text">qBittorrent URL</span></label>
				<input
					id="qbt-url"
					class="input input-bordered w-full font-mono text-sm"
					bind:value={url}
					placeholder="http://qbittorrent:8080"
				/>
			</div>
			<div class="form-control">
				<label class="label" for="qbt-key"><span class="label-text">API key</span></label>
				<div class="join w-full">
					<input
						id="qbt-key"
						type={showKey ? 'text' : 'password'}
						class="input input-bordered join-item flex-1 font-mono text-sm"
						bind:value={apiKey}
						placeholder="qBittorrent 5.2+ Web API key"
					/>
					<button
						type="button"
						class="btn join-item"
						onclick={() => (showKey = !showKey)}
						aria-label={showKey ? 'Hide API key' : 'Show API key'}
					>
						{showKey ? 'Hide' : 'Show'}
					</button>
				</div>
			</div>
			<div class="flex flex-wrap items-center gap-3">
				<button
					type="button"
					class="btn btn-outline btn-sm"
					onclick={onTest}
					disabled={test.isPending || !url || !apiKey}
				>
					{#if test.isPending}<span class="loading loading-spinner loading-xs"></span>{/if}
					Test connection
				</button>
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

		<div class="form-control">
			<label class="label" for="qbt-cat"><span class="label-text">Category</span></label>
			<input
				id="qbt-cat"
				class="input input-bordered w-full font-mono text-sm"
				bind:value={category}
				placeholder="droppedneedle"
			/>
			<span class="label">
				<span class="label-text-alt">
					DroppedNeedle scopes its torrents to this qBittorrent category. Create it in qBittorrent
					with its own save path. Imports <strong>copy</strong> files, so completed torrents keep seeding
					- private-tracker safe.
				</span>
			</span>
		</div>

		<section class="space-y-2">
			<div class="flex items-center gap-2 text-sm font-semibold">
				<FolderTree class="size-4 text-base-content/70" aria-hidden="true" /> Downloads mount
			</div>
			<div class="space-y-1.5 rounded-box border border-base-content/10 bg-base-200/40 p-3">
				<label class="text-sm font-medium" for="qbt-mount">Downloads mount</label>
				<p class="text-xs text-base-content/60">
					Where DroppedNeedle sees the category's save path (qBittorrent's completed folder),
					mounted read-only or read-write into this container.
				</p>
				<input
					id="qbt-mount"
					type="text"
					class="input input-sm input-bordered w-full font-mono"
					bind:value={downloadsMount}
					placeholder="/qbittorrent-downloads"
				/>
			</div>
		</section>

		<div class="flex justify-end">
			<button class="btn btn-primary" onclick={onSave} disabled={save.isPending}>
				{#if save.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
				Save settings
			</button>
		</div>
	</DownloadClientCard>
{/if}
