<script lang="ts">
	import { AlertTriangle } from 'lucide-svelte';
	import {
		getLibrarySettingsQuery,
		getLibraryStatsQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import { saveLibrarySettings } from '$lib/queries/library/LibraryMutations.svelte';
	import {
		getDownloadPolicyQuery,
		saveDownloadPolicy
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import LibraryPathEditor from '$lib/components/library/LibraryPathEditor.svelte';
	import LibraryNamingPreview from '$lib/components/library/LibraryNamingPreview.svelte';
	import LibraryScanButton from '$lib/components/library/LibraryScanButton.svelte';
	import LibraryForceRescanButton from '$lib/components/library/LibraryForceRescanButton.svelte';
	import LibraryScanScheduleControl from '$lib/components/library/LibraryScanScheduleControl.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';

	const settingsQuery = getLibrarySettingsQuery();
	const save = saveLibrarySettings();

	// Global storage cap (CollectionManagement Feature C): the value is download
	// policy, the usage side comes from the library stats. Admin-only - the policy
	// endpoint 403s for plain users, and only admins may set the cap.
	const policyQuery = getDownloadPolicyQuery(() => authStore.isAdmin);
	const savePolicy = saveDownloadPolicy();
	const statsQuery = getLibraryStatsQuery();

	let template = $state('');
	let acoustidKey = $state('');
	let lyricsFetch = $state(false);
	let seeded = $state(false);
	let maxLibraryGb = $state<number | null>(0);
	let capSeeded = $state(false);

	// Seed the editable fields once from the server; later refetches (after a
	// path add/remove) must not clobber unsaved template/key edits.
	$effect(() => {
		const d = settingsQuery.data;
		if (d && !seeded) {
			template = d.naming_template;
			acoustidKey = d.acoustid_api_key;
			lyricsFetch = d.lyrics_fetch_enabled;
			seeded = true;
		}
	});

	$effect(() => {
		const p = policyQuery.data;
		if (p && !capSeeded) {
			maxLibraryGb = p.max_library_size_gb;
			capSeeded = true;
		}
	});

	const usedBytes = $derived(statsQuery.data?.total_size_bytes ?? 0);
	const usedGb = $derived(usedBytes / 1024 ** 3);
	const capPercent = $derived(
		maxLibraryGb && maxLibraryGb > 0 ? Math.min(100, (usedGb / maxLibraryGb) * 100) : 0
	);

	async function handleSaveCap() {
		const p = policyQuery.data;
		if (!p) return;
		try {
			// a cleared number input binds null; treat it as 0 (= unlimited)
			await savePolicy.mutateAsync({ ...p, max_library_size_gb: maxLibraryGb ?? 0 });
			toastStore.show({ message: 'Storage cap saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save the storage cap', type: 'error' });
		}
	}

	const paths = $derived(settingsQuery.data?.library_paths ?? []);
	const hasKey = $derived(!!settingsQuery.data?.acoustid_api_key);

	async function handleSave() {
		const d = settingsQuery.data;
		if (!d) return;
		try {
			// Send the full record; the backend preserves the AcoustID key when the
			// masked sentinel is sent back unchanged.
			await save.mutateAsync({
				...d,
				library_paths: paths,
				naming_template: template,
				acoustid_api_key: acoustidKey,
				lyrics_fetch_enabled: lyricsFetch
			});
			toastStore.show({ message: 'Library settings saved', type: 'success' });
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Failed to save settings',
				type: 'error'
			});
		}
	}
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-xl font-bold">Library</h2>
		<p class="text-sm text-base-content/60">
			Manage your music library paths, file naming, and scanning.
		</p>
	</div>

	{#if settingsQuery.isLoading}
		<div class="skeleton h-64 w-full rounded-box"></div>
	{:else if settingsQuery.isError}
		<div class="alert alert-error">
			Failed to load library settings: {settingsQuery.error.message}
		</div>
	{:else}
		{#if !hasKey}
			<div class="alert alert-warning">
				<AlertTriangle class="h-5 w-5" />
				<span class="text-sm">
					No AcoustID key - fingerprint identification is off for files without MusicBrainz tags.
					Add a key to enable it.
				</span>
			</div>
		{/if}

		<section class="space-y-2">
			<h3 class="font-semibold">Library paths</h3>
			<LibraryPathEditor {paths} />
		</section>

		<section class="space-y-2">
			<h3 class="font-semibold">Naming template</h3>
			<p class="text-xs text-base-content/60">
				Applies to downloaded imports only. Variables: {'{albumartist} {album} {year} {disc} {track} {title} {ext}'}.
			</p>
			<input
				class="input input-bordered w-full font-mono text-sm"
				bind:value={template}
				aria-label="Naming template"
			/>
			<LibraryNamingPreview {template} />
		</section>

		<section class="space-y-2">
			<h3 class="font-semibold">AcoustID API key</h3>
			<input
				type="password"
				class="input input-bordered w-full font-mono text-sm"
				bind:value={acoustidKey}
				placeholder="Enter AcoustID API key"
				aria-label="AcoustID API key"
			/>
		</section>

		<section class="space-y-2">
			<h3
				class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
			>
				Lyrics
			</h3>
			<label class="flex w-fit cursor-pointer items-center gap-3">
				<input type="checkbox" class="toggle toggle-primary" bind:checked={lyricsFetch} />
				<span class="text-sm">Fetch missing lyrics from LRCLIB</span>
			</label>
			<p class="text-xs text-base-content/60">
				When a track has no embedded lyrics or .lrc file, DroppedNeedle asks LRCLIB (free, open
				lyrics database) once and saves a .lrc beside the file. Off = fully offline.
			</p>
		</section>

		<div>
			<button class="btn btn-primary" onclick={handleSave} disabled={save.isPending}>
				{#if save.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
				Save settings
			</button>
		</div>

		{#if authStore.isAdmin}
			<div class="divider my-1"></div>

			<section class="space-y-2">
				<h3 class="font-semibold">Storage cap</h3>
				<p class="text-xs text-base-content/60">
					Block new downloads once the library reaches this size. 0 = unlimited. Nothing is ever
					deleted, and quality upgrades are exempt since they replace existing files.
				</p>
				<div class="flex flex-wrap items-end gap-3">
					<label class="form-control">
						<span class="label-text text-xs">Maximum library size (GB)</span>
						<input
							type="number"
							min="0"
							max="1000000"
							class="input input-bordered input-sm w-40"
							bind:value={maxLibraryGb}
						/>
					</label>
					<button
						class="btn btn-primary btn-sm"
						onclick={handleSaveCap}
						disabled={savePolicy.isPending || !capSeeded}
					>
						Save cap
					</button>
				</div>
				{#if maxLibraryGb && maxLibraryGb > 0}
					<div class="max-w-md space-y-1">
						<progress
							class="progress w-full {capPercent >= 100
								? 'progress-error'
								: capPercent >= 85
									? 'progress-warning'
									: 'progress-primary'}"
							value={capPercent}
							max="100"
						></progress>
						<p class="text-xs text-base-content/60">
							{usedGb.toFixed(1)} GB of {maxLibraryGb} GB used
							{#if capPercent >= 100}
								- new downloads are blocked
							{/if}
						</p>
					</div>
				{:else}
					<p class="text-xs text-base-content/40">
						Library size: {usedGb.toFixed(1)} GB (no cap set)
					</p>
				{/if}
			</section>
		{/if}

		<div class="divider my-1"></div>

		<section class="space-y-2">
			<h3 class="font-semibold">Automatic scanning</h3>
			<p class="text-xs text-base-content/60">
				How often DroppedNeedle re-scans your library for new and changed files.
			</p>
			<LibraryScanScheduleControl />
		</section>

		<section class="space-y-2">
			<h3 class="font-semibold">Manual scan</h3>
			<div class="flex flex-wrap items-center gap-2">
				<LibraryScanButton
					hasPath={paths.length > 0}
					disabled={paths.length === 0}
					class="btn btn-outline gap-1"
					title={paths.length === 0 ? 'Add a library path first' : 'Start a library scan'}
				/>
				<LibraryForceRescanButton hasPath={paths.length > 0} />
			</div>
		</section>
	{/if}
</div>
