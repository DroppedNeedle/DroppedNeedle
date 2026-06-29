<script lang="ts">
	import {
		CircleCheck,
		CircleX,
		FolderTree,
		HardDriveDownload,
		Info,
		ShieldCheck,
		TriangleAlert
	} from 'lucide-svelte';

	import {
		getDownloadClientConfigQuery,
		getDownloadClientStatusQuery,
		saveDownloadClientConfig,
		testDownloadClient
	} from '$lib/queries/downloads/DownloadClientQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { DownloadClientConfig, TestConnectionResult } from '$lib/types';

	import QualityRangeSlider from './QualityRangeSlider.svelte';

	const configQuery = getDownloadClientConfigQuery();
	const statusQuery = getDownloadClientStatusQuery();
	const save = saveDownloadClientConfig();
	const test = testDownloadClient();

	let enabled = $state(false);
	let url = $state('');
	let apiKey = $state('');
	let showKey = $state(false);
	let verifyDownloads = $state(true);
	let qualityMin = $state('mp3_320');
	let qualityMax = $state('lossless');
	let flacMp3Only = $state(true);
	let downloadsSubpath = $state('');
	let autoAccept = $state(0.7);
	let manualMin = $state(0.5);
	let stallTimeout = $state(30);
	let queuedTimeout = $state(120);
	let maxFailover = $state(3);
	let maxConcurrent = $state(3);
	let seeded = $state(false);
	let testResult = $state<TestConnectionResult | null>(null);

	$effect(() => {
		const d = configQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			url = d.url;
			apiKey = d.api_key;
			verifyDownloads = d.verify_downloads;
			qualityMin = d.quality_min ?? 'mp3_320';
			qualityMax = d.quality_max ?? 'lossless';
			flacMp3Only = d.flac_mp3_only ?? true;
			downloadsSubpath = d.downloads_subpath ?? '';
			autoAccept = d.preflight_score_auto_accept;
			manualMin = d.preflight_score_manual_min;
			stallTimeout = d.download_stall_timeout_minutes ?? 30;
			queuedTimeout = d.download_queued_timeout_minutes ?? 120;
			maxFailover = d.max_failover_attempts ?? 3;
			maxConcurrent = d.max_concurrent_downloads ?? 3;
			seeded = true;
		}
	});

	const status = $derived(statusQuery.data);
	const connected = $derived(status?.configured === true && status?.client.status === 'ok');
	const mount = $derived(status?.mount);
	// Set when the mount looks reachable but slskd's finished downloads aren't visible
	// on it (wrong path / unreadable) - the silent misconfig.
	const mountAdvisory = $derived(status?.mount_advisory);
	// slskd's own configured downloads path - the hint for the subfolder fix below.
	const slskdDownloadsDir = $derived(status?.slskd_downloads_dir);

	const MOUNT_REASONS: Record<string, string> = {
		not_set: 'No slskd downloads folder is mounted into DroppedNeedle.',
		missing: "The mounted downloads folder doesn't exist.",
		not_writable: 'The downloads mount is read-only - imports MOVE files, so it must be writable.'
	};

	function currentConfig(): DownloadClientConfig | null {
		const d = configQuery.data;
		if (!d) return null;
		return {
			...d,
			enabled,
			url,
			api_key: apiKey,
			verify_downloads: verifyDownloads,
			quality_min: qualityMin,
			quality_max: qualityMax,
			flac_mp3_only: flacMp3Only,
			downloads_subpath: downloadsSubpath,
			preflight_score_auto_accept: autoAccept,
			preflight_score_manual_min: manualMin,
			download_stall_timeout_minutes: stallTimeout,
			download_queued_timeout_minutes: queuedTimeout,
			max_failover_attempts: maxFailover,
			max_concurrent_downloads: maxConcurrent
		};
	}

	async function handleTest() {
		testResult = null;
		const config = currentConfig();
		if (!config) return;
		try {
			testResult = await test.mutateAsync(config);
		} catch (e) {
			testResult = { valid: false, message: e instanceof Error ? e.message : 'Connection failed' };
		}
	}

	async function handleSave() {
		const config = currentConfig();
		if (!config) return;
		try {
			await save.mutateAsync(config);
			toastStore.show({ message: 'Download client saved', type: 'success' });
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Failed to save download client',
				type: 'error'
			});
		}
	}
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-xl font-bold">Download Client</h2>
		<p class="text-sm text-base-content/60">
			Connect a download client so DroppedNeedle can fetch requested albums and tracks.
		</p>
	</div>

	{#if configQuery.isLoading}
		<div class="skeleton h-96 w-full rounded-box"></div>
	{:else if configQuery.isError}
		<div class="alert alert-error">
			Failed to load download client settings: {configQuery.error.message}
		</div>
	{:else}
		<div class="client-card card border border-base-300 bg-base-200" class:is-active={enabled}>
			<div class="card-body gap-5">
				<div class="flex flex-wrap items-center gap-4">
					<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
						<HardDriveDownload class="size-6 text-accent" aria-hidden="true" />
					</div>
					<div class="min-w-0 flex-1">
						<div class="flex items-center gap-2">
							<h3 class="text-lg font-bold">slskd</h3>
							<span class="badge badge-ghost badge-sm">Soulseek</span>
						</div>
						<div class="flex items-center gap-2 text-sm text-base-content/70">
							<span
								class="orb"
								class:is-connected={connected}
								role="status"
								aria-label={connected ? 'Connected' : 'Not connected'}
							></span>
							{#if connected}
								Connected{status?.client.version ? ` · v${status.client.version}` : ''}
							{:else if status?.configured}
								{status?.client.message ?? 'Not reachable'}
							{:else}
								Not configured
							{/if}
						</div>
					</div>
					<label class="flex cursor-pointer items-center gap-2">
						<span class="text-sm font-medium">{enabled ? 'Enabled' : 'Disabled'}</span>
						<input
							type="checkbox"
							class="toggle toggle-accent"
							bind:checked={enabled}
							aria-label="Enable slskd download client"
						/>
					</label>
				</div>

				<section class="space-y-3">
					<div class="form-control">
						<label class="label" for="slskd-url"><span class="label-text">slskd URL</span></label>
						<input
							id="slskd-url"
							class="input input-bordered w-full font-mono text-sm"
							bind:value={url}
							placeholder="http://slskd:5030"
						/>
					</div>
					<div class="form-control">
						<label class="label" for="slskd-key"><span class="label-text">API key</span></label>
						<div class="join w-full">
							<input
								id="slskd-key"
								type={showKey ? 'text' : 'password'}
								class="input input-bordered join-item flex-1 font-mono text-sm"
								bind:value={apiKey}
								placeholder="slskd API key"
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
							onclick={handleTest}
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
					DroppedNeedle only orchestrates your own slskd instance over its local HTTP API; it
					never joins or distributes on the Soulseek network. You supply, run, and are responsible
					for slskd and its shared folders.
				</p>

				<div class="alert alert-warning items-start text-sm">
					<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
					<span>
						<span class="font-semibold">Share files in slskd.</span> Soulseek bans leechers -
						configure shared directories in <code>slskd.yml</code> so you can keep downloading.
					</span>
				</div>

				<section class="space-y-2">
					<div class="flex items-center gap-2 text-sm font-semibold">
						<FolderTree class="size-4 text-base-content/70" aria-hidden="true" /> Downloads mount
					</div>
					{#if mount?.ok && mountAdvisory}
						<div class="alert alert-warning items-start text-sm">
							<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
							<div class="space-y-1">
								<p>{mountAdvisory}</p>
								{#if mount.path}<code class="text-base-content/60">{mount.path}</code>{/if}
							</div>
						</div>
						<div class="space-y-1.5 rounded-box border border-base-content/10 bg-base-200/40 p-3">
							<label class="text-sm font-medium" for="downloads-subpath">Downloads subfolder</label>
							<p class="text-xs text-base-content/60">
								The folder inside your mount where slskd saves completed downloads.
								{#if slskdDownloadsDir}
									slskd saves to <code class="text-base-content/70">{slskdDownloadsDir}</code>. Set this
									to the part of that path inside your mount.
								{/if}
							</p>
							<div class="flex flex-wrap items-center gap-2">
								<input
									id="downloads-subpath"
									type="text"
									class="input input-sm input-bordered flex-1 font-mono"
									placeholder="e.g. downloads/slskd/complete"
									bind:value={downloadsSubpath}
								/>
								<button
									class="btn btn-sm btn-primary"
									onclick={handleSave}
									disabled={save.isPending}
								>
									{save.isPending ? 'Checking…' : 'Save & re-check'}
								</button>
							</div>
						</div>
					{:else if mount?.ok}
						<div class="flex items-center gap-2 text-sm text-success">
							<CircleCheck class="size-4" aria-hidden="true" />
							Reachable on the same disk as your library
							{#if mount.path}<code class="text-base-content/60">{mount.path}</code>{/if}
						</div>
					{:else if mount?.reason === 'different_filesystem'}
						<div class="alert alert-info items-start text-sm">
							<Info class="size-5 shrink-0" aria-hidden="true" />
							<div class="space-y-1">
								<p>
									Your downloads and library are on different filesystems. Imports still work -
									DroppedNeedle copies each file into the library instead of moving it instantly, so
									it's a bit slower and briefly needs room for both copies.
								</p>
								<p class="text-base-content/70">
									For instant moves, keep slskd's downloads and your music library on the same
									filesystem.
									{#if mount.path}<code class="text-base-content/60">{mount.path}</code>{/if}
								</p>
							</div>
						</div>
					{:else if mount}
						<div class="alert alert-warning items-start text-sm">
							<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
							<div class="space-y-1">
								<p>
									DroppedNeedle can't reach slskd's downloads folder, so finished downloads won't
									import.
									{MOUNT_REASONS[mount.reason] ?? mount.reason}
								</p>
								<details class="text-xs">
									<summary class="cursor-pointer font-semibold">How to set this up</summary>
									<p class="mt-1 text-base-content/70">
										Mount slskd's download directory into DroppedNeedle <strong>read-write</strong>, on
										the
										<strong>same disk</strong> as your music library (imports move files with an
										atomic rename). See the slskd setup guide (<code>docs/SLSKD_SETUP.md</code>).
									</p>
								</details>
							</div>
						</div>
					{/if}
				</section>

				<section class="space-y-4 rounded-box bg-base-100/40 p-4">
					<div class="flex items-center gap-2 text-sm font-semibold">
						<ShieldCheck class="size-4 text-base-content/70" aria-hidden="true" /> Download Quality &amp;
						Verification
					</div>
					<div class="space-y-3">
						<span class="text-sm font-medium">Preferred quality</span>
						<QualityRangeSlider bind:minKey={qualityMin} bind:maxKey={qualityMax} />
						<label class="flex items-start gap-3 pt-1">
							<input
								type="checkbox"
								class="toggle toggle-accent mt-0.5"
								bind:checked={flacMp3Only}
							/>
							<span class="text-sm">
								<span class="font-medium">Only FLAC &amp; MP3</span>
								<span class="block text-base-content/50">
									Skip other lossy formats like AAC/M4A, OGG and Opus, even when they fall in range.
								</span>
							</span>
						</label>
					</div>
					<div class="grid gap-4 sm:grid-cols-2">
						<label class="flex items-start gap-3">
							<input
								type="checkbox"
								class="toggle toggle-accent mt-1"
								bind:checked={verifyDownloads}
							/>
							<span class="text-sm">
								<span class="font-medium">Verify downloads with AcoustID</span>
								<span class="block text-base-content/50">
									Fingerprint finished files and quarantine confident mismatches. Needs an AcoustID
									key; fail-open - errors never block an import.
								</span>
							</span>
						</label>
						<div class="form-control">
							<label class="label" for="auto-accept">
								<span class="label-text">Auto-accept score</span>
							</label>
							<input
								id="auto-accept"
								type="number"
								min="0"
								max="1"
								step="0.05"
								class="input input-bordered"
								bind:value={autoAccept}
							/>
							<span class="label-text-alt mt-1 text-base-content/50">
								Candidates at or above this score download automatically.
							</span>
						</div>
						<div class="form-control">
							<label class="label" for="manual-min">
								<span class="label-text">Manual-review score</span>
							</label>
							<input
								id="manual-min"
								type="number"
								min="0"
								max="1"
								step="0.05"
								class="input input-bordered"
								bind:value={manualMin}
							/>
							<span class="label-text-alt mt-1 text-base-content/50">
								Below auto-accept and above this lands in the Review tab.
							</span>
						</div>
					</div>
				</section>

				<section class="space-y-3">
					<h4 class="text-sm font-semibold">Download resilience</h4>
					<p class="text-xs text-base-content/60">
						How long to wait on a slow Soulseek peer before trying another source, and how
						many downloads run at once. The defaults suit most setups.
					</p>
					<div class="grid gap-3 sm:grid-cols-2">
					<div class="form-control">
						<label class="label" for="stall-timeout"><span class="label-text">Stall timeout (minutes)</span></label>
						<input
							id="stall-timeout"
							type="number"
							min="2"
							max="240"
							step="1"
							class="input input-bordered"
							bind:value={stallTimeout}
						/>
						<span class="label-text-alt mt-1 text-base-content/50">No download progress for this long on an active transfer counts as stalled.</span>
					</div>
					<div class="form-control">
						<label class="label" for="queued-timeout"><span class="label-text">Queue wait timeout (minutes)</span></label>
						<input
							id="queued-timeout"
							type="number"
							min="5"
							max="1440"
							step="5"
							class="input input-bordered"
							bind:value={queuedTimeout}
						/>
						<span class="label-text-alt mt-1 text-base-content/50">How long to wait in a peer's upload queue before giving up on that source.</span>
					</div>
					<div class="form-control">
						<label class="label" for="max-failover"><span class="label-text">Failover attempts</span></label>
						<input
							id="max-failover"
							type="number"
							min="1"
							max="10"
							step="1"
							class="input input-bordered"
							bind:value={maxFailover}
						/>
						<span class="label-text-alt mt-1 text-base-content/50">How many alternate sources to try before a download is marked failed.</span>
					</div>
					<div class="form-control">
						<label class="label" for="max-concurrent"><span class="label-text">Concurrent downloads</span></label>
						<input
							id="max-concurrent"
							type="number"
							min="1"
							max="10"
							step="1"
							class="input input-bordered"
							bind:value={maxConcurrent}
						/>
						<span class="label-text-alt mt-1 text-base-content/50">Maximum albums actively downloading at once.</span>
					</div>
					</div>
				</section>

				<div class="flex justify-end">
					<button class="btn btn-primary" onclick={handleSave} disabled={save.isPending}>
						{#if save.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
						Save settings
					</button>
				</div>
			</div>
		</div>
	{/if}
</div>

<style>
	.client-card {
		transition:
			box-shadow 0.4s ease,
			border-color 0.4s ease;
	}
	.client-card.is-active {
		border-color: oklch(from var(--color-accent) l c h / 0.55);
		box-shadow:
			0 0 0 1px oklch(from var(--color-accent) l c h / 0.3),
			0 0 44px oklch(from var(--color-accent) l c h / 0.18);
	}

	.orb {
		display: inline-block;
		width: 0.7rem;
		height: 0.7rem;
		border-radius: 9999px;
		background: oklch(from var(--color-base-content) l c h / 0.3);
		transition: background 0.3s ease;
	}
	.orb.is-connected {
		background: var(--color-accent);
		animation: orb-pulse 2.4s ease-in-out infinite;
	}
	@keyframes orb-pulse {
		0%,
		100% {
			box-shadow: 0 0 5px oklch(from var(--color-accent) l c h / 0.5);
		}
		50% {
			box-shadow: 0 0 14px oklch(from var(--color-accent) l c h / 0.95);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.orb.is-connected {
			animation: none;
		}
	}
</style>
