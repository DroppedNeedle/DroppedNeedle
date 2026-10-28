<script lang="ts">
	import { Check, Download } from 'lucide-svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { createDiscoveryBatchV3 } from '$lib/queries/discover/DiscoverV3Mutations.svelte';
	import type { AlbumRef } from '$lib/queries/discover/types';

	interface Props {
		open: boolean;
		sectionTitle: string;
		sectionKey: string;
		albums: AlbumRef[];
	}

	let { open = $bindable(false), sectionTitle, sectionKey, albums }: Props = $props();

	let name = $state('');
	let deselected = new SvelteSet<string>();
	let submitting = $state(false);

	const eligible = $derived(
		albums.filter(
			(a, i) =>
				a.musicbrainz_id && albums.findIndex((b) => b.musicbrainz_id === a.musicbrainz_id) === i
		)
	);
	const isRequested = (album: AlbumRef) =>
		!!album.requested || libraryStore.isRequested(album.musicbrainz_id);
	const selectedCount = $derived(
		eligible.filter((a) => !a.in_library && !isRequested(a) && !deselected.has(a.musicbrainz_id!))
			.length
	);
	const needsApproval = $derived(authStore.user?.role === 'user');

	$effect(() => {
		if (open) {
			name = `${sectionTitle} - ${new Date().toLocaleDateString(undefined, {
				month: 'short',
				day: 'numeric'
			})}`;
			deselected.clear();
			for (const album of eligible.filter((item) => item.in_library || isRequested(item))) {
				deselected.add(album.musicbrainz_id!);
			}
		}
	});

	function toggle(mbid: string) {
		if (deselected.has(mbid)) {
			deselected.delete(mbid);
		} else {
			deselected.add(mbid);
		}
	}

	async function submit() {
		if (submitting || selectedCount === 0) return;
		submitting = true;
		try {
			const items = eligible
				.filter((a) => !a.in_library && !isRequested(a) && !deselected.has(a.musicbrainz_id!))
				.map((a) => ({
					musicbrainz_id: a.musicbrainz_id!,
					artist_id: a.artist_id ?? '',
					title: a.title,
					artist_name: a.artist_name ?? ''
				}));
			const created = await createDiscoveryBatchV3({
				name: name.trim() || sectionTitle,
				source_section: sectionKey,
				items
			});
			if (created) open = false;
		} finally {
			submitting = false;
		}
	}
</script>

<Modal
	bind:open
	title="Download this section"
	description={needsApproval
		? 'Requests each selected album using the same rules as an individual request. These will wait for admin approval. You can remove the batch later from Downloads.'
		: 'Requests each selected album using the same rules as an individual request. You can remove the batch later from Downloads.'}
	size="lg"
	closeButton
>
	<div class="space-y-4">
		<TextField label="Batch name" bind:value={name} />

		<div class="max-h-80 space-y-1 overflow-y-auto pr-1">
			{#each eligible as album (album.musicbrainz_id)}
				<label
					class="flex items-center gap-3 rounded-lg px-2 py-1.5 {album.in_library ||
					isRequested(album)
						? 'opacity-60'
						: 'cursor-pointer hover:bg-base-content/5'}"
				>
					<input
						type="checkbox"
						class="checkbox checkbox-sm"
						checked={!deselected.has(album.musicbrainz_id!)}
						disabled={album.in_library || isRequested(album)}
						onchange={() => toggle(album.musicbrainz_id!)}
					/>
					<AlbumImage
						mbid={album.musicbrainz_id!}
						alt={album.title}
						size="full"
						requestSize={250}
						lazy={true}
						rounded="md"
						className="block h-10 w-10 shrink-0 object-cover"
						customUrl={album.cover_url || null}
					/>
					<div class="min-w-0 flex-1">
						<p class="truncate text-sm font-medium">{album.title}</p>
						<p class="truncate text-xs text-base-content/50">{album.artist_name}</p>
					</div>
					{#if album.in_library}
						<span class="badge badge-success badge-sm gap-1 shrink-0">
							<Check class="h-3 w-3" /> Owned
						</span>
					{:else if isRequested(album)}
						<span class="badge badge-info badge-sm shrink-0">Requested</span>
					{/if}
				</label>
			{/each}
		</div>
	</div>

	{#snippet actions()}
		<Button variant="ghost" size="sm" onclick={() => (open = false)}>Cancel</Button>
		<Button
			variant="primary"
			size="sm"
			icon={Download}
			loading={submitting}
			disabled={selectedCount === 0}
			onclick={submit}
		>
			Request {selectedCount} album{selectedCount === 1 ? '' : 's'}
		</Button>
	{/snippet}
</Modal>
