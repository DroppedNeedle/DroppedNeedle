<script lang="ts">
	/*
	 * AlbumTile — the new artwork-first album tile.
	 *
	 * No card box: large rounded artwork, text beneath, actions revealed on
	 * hover/focus (always visible on touch). Preserves every behaviour of the old
	 * carded tile: album link, in-library chip, overlay play (in-library), request
	 * button, 30s preview, and the search fallback when there is no MBID.
	 */
	import { goto } from '$app/navigation';
	import { Check, Search } from 'lucide-svelte';
	import { albumHrefOrNull } from '$lib/utils/entityRoutes';
	import { integrationStore } from '$lib/stores/integration';
	import { libraryStore } from '$lib/stores/library';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import AlbumCardOverlay from '$lib/components/AlbumCardOverlay.svelte';
	import AlbumRequestButton from '$lib/components/AlbumRequestButton.svelte';
	import SampleButton from '$lib/components/discover/SampleButton.svelte';

	interface Props {
		mbid?: string | null;
		name: string;
		artistName?: string | null;
		artistMbid?: string | null;
		imageUrl?: string | null;
		inLibrary?: boolean;
		requested?: boolean;
		showPreview?: boolean;
		subtitle?: string | null;
	}

	let {
		mbid = null,
		name,
		artistName = null,
		artistMbid = null,
		imageUrl = null,
		inLibrary = false,
		requested = false,
		showPreview = true,
		subtitle = null
	}: Props = $props();

	const href = $derived(albumHrefOrNull(mbid));
	const isRequested = $derived(requested || (mbid ? libraryStore.isRequested(mbid) : false));
	const canRequest = $derived(
		!!mbid && $integrationStore.download_client && !inLibrary && !isRequested
	);

	function searchAlbum(e: MouseEvent): void {
		e.stopPropagation();
		e.preventDefault();
		const query = [artistName, name].filter(Boolean).join(' ').trim();
		if (query) goto(`/search/albums?q=${encodeURIComponent(query)}`);
	}
</script>

<div class="dn-tile group">
	<svelte:element
		this={href ? 'a' : 'div'}
		href={href ?? undefined}
		data-sveltekit-preload-data={href ? 'hover' : undefined}
		class="dn-tile__link {href ? '' : 'dn-tile__link--static'}"
	>
		<figure class="dn-tile__art">
			<AlbumImage
				mbid={mbid || ''}
				alt={name}
				size="md"
				rounded="none"
				className="h-full w-full"
				customUrl={imageUrl || null}
			/>
			{#if inLibrary}
				<span class="dn-tile__chip" title="In your library"><Check class="h-3 w-3" /></span>
			{/if}
			{#if mbid && inLibrary}
				<AlbumCardOverlay
					{mbid}
					albumName={name}
					artistName={artistName || 'Unknown'}
					coverUrl={imageUrl || null}
					size="sm"
				/>
			{/if}
			{#if !mbid}
				<button
					type="button"
					class="dn-tile__fallback"
					title="Search for this album"
					onclick={searchAlbum}
				>
					<Search class="h-3.5 w-3.5" />
				</button>
			{/if}
		</figure>
		<div class="dn-tile__meta">
			<h3 class="dn-tile__title">{name}</h3>
			{#if artistName}
				<p class="dn-tile__sub">{artistName}</p>
			{:else if subtitle}
				<p class="dn-tile__sub">{subtitle}</p>
			{/if}
		</div>
	</svelte:element>

	{#if mbid && (canRequest || showPreview)}
		<div class="dn-tile__actions">
			{#if canRequest}
				<AlbumRequestButton
					{mbid}
					artistName={artistName ?? ''}
					albumName={name}
					artistMbid={artistMbid ?? undefined}
				/>
			{/if}
			{#if showPreview}
				<SampleButton
					sampleKey={mbid}
					artist={artistName ?? ''}
					title={name}
					kind="album"
					size="sm"
					artistMbid={artistMbid ?? undefined}
					coverUrl={imageUrl ?? undefined}
				/>
			{/if}
		</div>
	{/if}
</div>
