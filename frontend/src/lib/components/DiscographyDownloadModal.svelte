<script lang="ts">
	import { Download, X, Disc3, Check, LoaderCircle, Library } from 'lucide-svelte';
	import { discographyDownloadStore } from '$lib/stores/discographyDownload.svelte';
	import { batchDownloadStore } from '$lib/stores/batchDownloadStatus.svelte';
	import {
		requestBatch,
		type BatchAlbumItem
	} from '$lib/queries/downloads/DownloadMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	let dialogEl: HTMLDialogElement | undefined = $state();
	let submitting = $state(false);
	let includeAlbums = $state(true);
	let includeEPs = $state(true);
	let includeSingles = $state(true);
	let monitorArtist = $state(false);
	let autoDownload = $state(false);

	const batchRequest = requestBatch();
	$effect(() => {
		if (discographyDownloadStore.open) {
			includeAlbums = true;
			includeEPs = true;
			includeSingles = true;
			monitorArtist = false;
			autoDownload = false;
			submitting = false;
		}
	});

	let filteredReleases = $derived.by(() => {
		const types = new Set<string>(); // eslint-disable-line svelte/prefer-svelte-reactivity
		if (includeAlbums) types.add('Album');
		if (includeEPs) types.add('EP');
		if (includeSingles) types.add('Single');
		return discographyDownloadStore.releases.filter(
			(r) => types.has(r.type ?? 'Album') && !r.in_library && !r.requested
		);
	});

	let inLibraryCount = $derived(
		discographyDownloadStore.releases.filter((r) => r.in_library).length
	);
	let requestedCount = $derived(
		discographyDownloadStore.releases.filter((r) => r.requested && !r.in_library).length
	);
	let totalReleases = $derived(discographyDownloadStore.releases.length);

	let albumCount = $derived(
		discographyDownloadStore.releases.filter((r) => (r.type ?? 'Album') === 'Album').length
	);
	let epCount = $derived(discographyDownloadStore.releases.filter((r) => r.type === 'EP').length);
	let singleCount = $derived(
		discographyDownloadStore.releases.filter((r) => r.type === 'Single').length
	);

	$effect(() => {
		if (discographyDownloadStore.open && dialogEl) {
			dialogEl.showModal();
		} else if (!discographyDownloadStore.open && dialogEl?.open) {
			dialogEl.close();
		}
	});

	function handleClose() {
		dialogEl?.close();
		discographyDownloadStore.close();
	}

	async function handleDownload() {
		if (filteredReleases.length === 0) return;
		const artistName = discographyDownloadStore.artistName;
		const artistId = discographyDownloadStore.artistId;
		const initiatingUserId = authStore.user?.id;
		submitting = true;

		const items: BatchAlbumItem[] = filteredReleases.map((r) => ({
			musicbrainz_id: r.id,
			artist_name: artistName,
			album_title: r.title,
			year: r.year ?? undefined,
			artist_mbid: artistId
		}));

		const result = await batchRequest
			.mutateAsync({
				items,
				monitorArtist,
				autoDownloadArtist: autoDownload
			})
			.catch(() => null);
		// an in-flight batch that resolves after an account switch belongs to the prior
		// session - the shell already cleared the stores, do not re-add the old job (#155)
		if (result?.success && authStore.user?.id === initiatingUserId) {
			batchDownloadStore.addJob(
				artistName,
				artistId,
				items.map((i) => i.musicbrainz_id)
			);
			handleClose();
		}

		submitting = false;
	}
</script>

{#if discographyDownloadStore.open}
	<dialog
		bind:this={dialogEl}
		class="modal"
		aria-labelledby="discography-download-title"
		onclose={handleClose}
	>
		<div class="modal-box max-w-lg">
			<div class="flex items-center justify-between mb-4">
				<div class="flex items-center gap-3">
					<div class="bg-accent/10 rounded-full p-2">
						<Download class="h-5 w-5 text-accent" />
					</div>
					<div>
						<h3 id="discography-download-title" class="text-lg font-bold">Download Discography</h3>
						<p class="text-sm text-base-content/60">{discographyDownloadStore.artistName}</p>
					</div>
				</div>
				<IconButton size="sm" icon={X} label="Close" onclick={handleClose} />
			</div>

			<div class="flex gap-2 flex-wrap mb-4">
				<div class="badge badge-ghost gap-1">
					<Disc3 class="h-3 w-3" />
					{totalReleases} total
				</div>
				{#if inLibraryCount > 0}
					<div class="badge badge-success gap-1">
						<Check class="h-3 w-3" />
						{inLibraryCount} in library
					</div>
				{/if}
				{#if requestedCount > 0}
					<div class="badge badge-info gap-1">
						<LoaderCircle class="h-3 w-3" />
						{requestedCount} requested
					</div>
				{/if}
			</div>

			<Card padding="sm" class="mb-4">
				<p class="text-xs font-medium text-base-content/50 uppercase tracking-wider">Include</p>
				<div class="flex flex-wrap gap-3">
					{#if albumCount > 0}
						<label class="label cursor-pointer gap-2 p-0">
							<input type="checkbox" class="checkbox checkbox-sm" bind:checked={includeAlbums} />
							<span class="text-sm">Albums ({albumCount})</span>
						</label>
					{/if}
					{#if epCount > 0}
						<label class="label cursor-pointer gap-2 p-0">
							<input type="checkbox" class="checkbox checkbox-sm" bind:checked={includeEPs} />
							<span class="text-sm">EPs ({epCount})</span>
						</label>
					{/if}
					{#if singleCount > 0}
						<label class="label cursor-pointer gap-2 p-0">
							<input type="checkbox" class="checkbox checkbox-sm" bind:checked={includeSingles} />
							<span class="text-sm">Singles ({singleCount})</span>
						</label>
					{/if}
				</div>
			</Card>

			{#if filteredReleases.length > 0}
				<div class="mb-4">
					<p class="text-xs font-medium text-base-content/50 uppercase tracking-wider mb-2">
						{filteredReleases.length} album{filteredReleases.length !== 1 ? 's' : ''} to request
					</p>
					<div
						class="grid grid-cols-[repeat(auto-fill,minmax(3.5rem,1fr))] gap-1.5 max-h-48 overflow-y-auto pr-1"
					>
						{#each filteredReleases.slice(0, 40) as release (release.id)}
							<div
								class="aspect-square rounded-lg overflow-hidden relative group"
								title={release.title}
							>
								<AlbumImage
									mbid={release.id}
									alt={release.title}
									size="sm"
									rounded="lg"
									className="w-full h-full"
								/>
								<div
									class="absolute inset-0 bg-black/60 opacity-0 group-hover:opacity-100 transition-opacity flex items-center justify-center"
								>
									<span
										class="text-[0.6rem] text-white text-center px-1 line-clamp-2 leading-tight"
									>
										{release.title}
									</span>
								</div>
							</div>
						{/each}
						{#if filteredReleases.length > 40}
							<div class="aspect-square rounded-lg bg-base-300 flex items-center justify-center">
								<span class="text-xs text-base-content/50">+{filteredReleases.length - 40}</span>
							</div>
						{/if}
					</div>
				</div>
			{:else}
				<Card padding="md" class="text-center mb-4">
					<Library class="h-8 w-8 mx-auto text-base-content/30" />
					<p class="text-sm text-base-content/50">
						{#if inLibraryCount === totalReleases}
							All releases are already in your library
						{:else}
							No releases to download with current filters
						{/if}
					</p>
				</Card>
			{/if}

			<Card padding="sm" class="mb-4">
				<p class="text-xs font-medium text-base-content/50 uppercase tracking-wider">Options</p>
				<label class="label cursor-pointer justify-start gap-3 p-0">
					<input
						type="checkbox"
						class="toggle toggle-sm toggle-primary"
						bind:checked={monitorArtist}
					/>
					<span class="text-sm">Monitor artist for future releases</span>
				</label>
				{#if monitorArtist}
					<label class="label cursor-pointer justify-start gap-3 p-0 pl-10">
						<input
							type="checkbox"
							class="toggle toggle-sm toggle-primary"
							bind:checked={autoDownload}
						/>
						<span class="text-sm">Auto-download new releases</span>
					</label>
				{/if}
			</Card>

			<div class="modal-action mt-0">
				<Button variant="ghost" onclick={handleClose} disabled={submitting}>Cancel</Button>
				<Button
					variant="accent"
					icon={Download}
					loading={submitting}
					disabled={filteredReleases.length === 0}
					onclick={handleDownload}
				>
					{#if submitting}
						Requesting...
					{:else}
						Download {filteredReleases.length} Album{filteredReleases.length !== 1 ? 's' : ''}
					{/if}
				</Button>
			</div>
		</div>
		<form method="dialog" class="modal-backdrop">
			<button>close</button>
		</form>
	</dialog>
{/if}
