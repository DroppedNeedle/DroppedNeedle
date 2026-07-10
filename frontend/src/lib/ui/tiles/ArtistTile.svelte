<script lang="ts">
	/*
	 * ArtistTile — circular artwork-first artist tile.
	 * Preserves: artist link, in-library chip, unlinked (no-MBID) marker,
	 * listen-count subtitle.
	 */
	import { Check, X } from 'lucide-svelte';
	import { artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { formatListenCount } from '$lib/utils/formatting';
	import ArtistImage from '$lib/components/ArtistImage.svelte';

	interface Props {
		mbid?: string | null;
		name: string;
		listenCount?: number | null;
		inLibrary?: boolean;
		subtitle?: string | null;
	}

	let {
		mbid = null,
		name,
		listenCount = null,
		inLibrary = false,
		subtitle = null
	}: Props = $props();

	const href = $derived(artistHrefOrNull(mbid));
</script>

<div class="dn-tile dn-tile--artist group">
	<svelte:element
		this={href ? 'a' : 'div'}
		href={href ?? undefined}
		data-sveltekit-preload-data={href ? 'hover' : undefined}
		class="dn-tile__link {href ? '' : 'dn-tile__link--static'}"
	>
		<figure class="dn-tile__art dn-tile__art--round">
			<ArtistImage mbid={mbid ?? ''} alt={name} size="md" lazy={true} className="h-full w-full" />
			{#if inLibrary}
				<span class="dn-tile__chip" title="In your library"><Check class="h-3 w-3" /></span>
			{/if}
			{#if !mbid}
				<span class="dn-tile__chip dn-tile__chip--muted" title="Not linked to MusicBrainz">
					<X class="h-3 w-3" />
				</span>
			{/if}
		</figure>
		<div class="dn-tile__meta dn-tile__meta--center">
			<h3 class="dn-tile__title">{name}</h3>
			{#if listenCount}
				<p class="dn-tile__sub">{formatListenCount(listenCount)}</p>
			{:else if subtitle}
				<p class="dn-tile__sub">{subtitle}</p>
			{/if}
		</div>
	</svelte:element>
</div>
