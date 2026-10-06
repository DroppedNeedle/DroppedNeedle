<script lang="ts">
	import { CircleCheck, CircleX, FolderTree, Rss, TriangleAlert } from 'lucide-svelte';

	import {
		getNzbgetConfigQuery,
		getNzbgetStatusQuery,
		getSabnzbdConfigQuery,
		saveNzbgetConfig,
		testNzbget
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { getIndexersQuery } from '$lib/queries/downloads/IndexerQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { NzbgetConnectionSettings, NzbgetTestResult } from '$lib/types';
	import { withBasePath } from '$lib/utils/basePath';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const configQuery = getNzbgetConfigQuery();
	const statusQuery = getNzbgetStatusQuery();
	const sabnzbdQuery = getSabnzbdConfigQuery();
	const indexersQuery = getIndexersQuery();
	const save = saveNzbgetConfig();
	const test = testNzbget();

	// NZBGet only downloads what an indexer finds - a Usenet source with no indexer is inert.
	const hasIndexer = $derived((indexersQuery.data?.length ?? 0) > 0);
	// One Usenet client runs at a time and SABnzbd wins a tie, so say so rather than
	// letting both cards read as enabled while only one receives downloads.
	const sabnzbdWins = $derived(sabnzbdQuery.data?.enabled === true);

	let enabled = $state(false);
	let url = $state('');
	let username = $state('nzbget');
	let password = $state('');
	let showPassword = $state(false);
	let category = $state('');
	let downloadsMount = $state('/nzbget-downloads');
	let priority = $state(0);
	let seeded = $state(false);
	let testResult = $state<NzbgetTestResult | null>(null);

	$effect(() => {
		const d = configQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			url = d.url;
			username = d.username || 'nzbget';
			password = d.password;
			category = d.category || '';
			downloadsMount = d.downloads_mount || '/nzbget-downloads';
			priority = d.priority ?? 0;
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

	function current(): NzbgetConnectionSettings {
		return {
			enabled,
			client_type: 'nzbget',
			url,
			username,
			password,
			category,
			priority,
			downloads_mount: downloadsMount
		};
	}

	async function onSave() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: 'NZBGet settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save NZBGet settings', type: 'error' });
		}
	}

	// The enable switch sits in the collapsed header, so persist it the moment it flips
	// rather than making the user expand the card and hit Save. Revert on failure.
	async function onToggle() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: `NZBGet ${enabled ? 'enabled' : 'disabled'}`, type: 'success' });
		} catch {
			enabled = !enabled;
			toastStore.show({ message: 'Could not update NZBGet', type: 'error' });
		}
	}

	async function onTest() {
		try {
			testResult = await test.mutateAsync(current());
		} catch {
			testResult = { valid: false, message: "Couldn't reach NZBGet", categories: [] };
		}
	}

	const categoryOptions = $derived(
		testResult?.categories?.length
			? testResult.categories
			: [...new Set([category].filter(Boolean))]
	);
</script>

{#if configQuery.isLoading}
	<div class="skeleton h-28 w-full rounded-box"></div>
{:else if configQuery.isError}
	<div class="alert alert-error">
		Failed to load NZBGet settings: {configQuery.error.message}
	</div>
{:else}
	<DownloadClientCard
		title="NZBGet"
		sourceLabel="Usenet"
		icon={Rss}
		{connected}
		{statusText}
		bind:enabled
		{onToggle}
		enableAriaLabel="Enable NZBGet download client"
	>
		{#if enabled && sabnzbdWins}
			<div class="alert alert-warning items-start text-sm">
				<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
				<div class="space-y-1">
					<p>
						<span class="font-semibold">SABnzbd is also enabled.</span> One Usenet client handles downloads
						at a time, and SABnzbd takes precedence. Disable it to send Usenet downloads here.
					</p>
				</div>
			</div>
		{/if}

		{#if enabled && !indexersQuery.isLoading && !hasIndexer}
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
			<div class="form-control">
				<label class="label" for="nzbget-url"><span class="label-text">NZBGet URL</span></label>
				<input
					id="nzbget-url"
					class="input input-bordered w-full font-mono text-sm"
					bind:value={url}
					placeholder="http://nzbget:6789"
				/>
			</div>
			<div class="form-control">
				<label class="label" for="nzbget-user"
					><span class="label-text">Control username</span></label
				>
				<input
					id="nzbget-user"
					class="input input-bordered w-full font-mono text-sm"
					bind:value={username}
					placeholder="nzbget"
				/>
			</div>
			<div class="form-control">
				<label class="label" for="nzbget-pass"
					><span class="label-text">Control password</span></label
				>
				<div class="join w-full">
					<input
						id="nzbget-pass"
						type={showPassword ? 'text' : 'password'}
						class="input input-bordered join-item flex-1 font-mono text-sm"
						bind:value={password}
						placeholder="NZBGet control password"
					/>
					<button
						type="button"
						class="btn join-item"
						onclick={() => (showPassword = !showPassword)}
						aria-label={showPassword ? 'Hide password' : 'Show password'}
					>
						{showPassword ? 'Hide' : 'Show'}
					</button>
				</div>
			</div>
			<div class="flex flex-wrap items-center gap-3">
				<button
					type="button"
					class="btn btn-outline btn-sm"
					onclick={onTest}
					disabled={test.isPending || !url}
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

		<p class="text-xs leading-relaxed text-base-content/60">
			NZBGet downloads the NZBs your indexers find. Use the <strong>control</strong> username and
			password from Settings → Security (<code>ControlUsername</code> and
			<code>ControlPassword</code>), not the restricted or add-only pair - those can't manage the
			queue. NZBGet 16 or newer is required.
		</p>

		<div class="form-control">
			<label class="label" for="nzbget-cat"><span class="label-text">Category</span></label>
			<select id="nzbget-cat" class="select select-bordered" bind:value={category}>
				<option value="">NZBGet default</option>
				{#each categoryOptions as opt (opt)}
					<option value={opt}>{opt}</option>
				{/each}
			</select>
			<span class="label">
				<span class="label-text-alt">
					Run Test to load categories. A dedicated <code>droppedneedle</code> category gives predictable
					folders, but the default works.
				</span>
			</span>
		</div>

		<section class="space-y-2">
			<div class="flex items-center gap-2 text-sm font-semibold">
				<FolderTree class="size-4 text-base-content/70" aria-hidden="true" /> Downloads mount
			</div>
			<div class="space-y-1.5 rounded-box border border-base-content/10 bg-base-200/40 p-3">
				<label class="text-sm font-medium" for="nzbget-mount">Downloads mount</label>
				<p class="text-xs text-base-content/60">
					Where DroppedNeedle sees NZBGet's completed folder (its <code>DestDir</code>), mounted
					read-write into this container on the same disk as your library.
					{#if testResult?.complete_dir}
						NZBGet's DestDir is <code class="text-base-content/70">{testResult.complete_dir}</code>.
					{/if}
				</p>
				<input
					id="nzbget-mount"
					type="text"
					class="input input-sm input-bordered w-full font-mono"
					bind:value={downloadsMount}
					placeholder="/nzbget-downloads"
				/>
				{#if testResult?.mount_message}
					<p class="text-xs leading-relaxed text-warning">{testResult.mount_message}</p>
				{/if}
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
