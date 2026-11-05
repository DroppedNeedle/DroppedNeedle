<!--
	The heart and the stars of the track playing, beside the player's
	controls. Only a library track has them; other sources show nothing.
	The star shows the listener's rating in it and opens a bar of five
	stars to change it.
-->
<script lang="ts">
	import { Heart, Star } from 'lucide-svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { createQuery } from '@tanstack/svelte-query';
	import { getLibraryAlbumStatusQueryOptions } from '$lib/features/library/queries/LibraryQueries.svelte';
	import { createSetFavoriteMutation } from '$lib/features/library/queries/FavoriteMutations.svelte';
	import {
		createSetRatingMutation,
		getRatingsQuery,
		ratingOf
	} from '$lib/features/library/queries/ratings.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StarPicker from './StarPicker.svelte';

	const trackId = $derived(
		playerStore.nowPlaying?.sourceType === 'local'
			? (playerStore.nowPlaying.trackSourceId ?? null)
			: null
	);

	// The player names the album by its local id or its release group; the
	// album's status answers to both and carries its tracks' hearts. It is
	// the album page's own read, so a heart given there or here shows in
	// both.
	const albumId = $derived(trackId ? (playerStore.nowPlaying?.albumId ?? '') : '');
	const statusQuery = createQuery(() => ({
		...getLibraryAlbumStatusQueryOptions(albumId),
		enabled: albumId !== ''
	}));
	const ratingsQuery = getRatingsQuery(() => (trackId ? { trackIds: [trackId] } : null));
	const favoriteMutation = createSetFavoriteMutation();
	const ratingMutation = createSetRatingMutation();

	const favorite = $derived(
		statusQuery.data?.tracks.find((track) => track.id === trackId)?.favorite ?? false
	);
	const stars = $derived(trackId ? ratingOf(ratingsQuery.data, 'track', trackId) : 0);
</script>

{#if trackId}
	<div class="hidden sm:flex items-center gap-1 ml-2 sm:ml-4">
		<IconButton
			icon={Heart}
			variant={favorite ? 'danger-ghost' : 'ghost'}
			label={favorite ? 'Remove from favourites' : 'Add to favourites'}
			aria-pressed={favorite}
			class={favorite ? '[&_svg]:fill-current' : ''}
			onclick={() =>
				favoriteMutation.mutate({ kind: 'track', itemId: trackId, favorited: !favorite })}
		/>
		<StarPicker
			label={stars > 0 ? `Your rating: ${stars} of 5 stars` : 'Rate this track'}
			value={stars}
			clear="remove"
			active={stars > 0}
			onpick={(picked) => ratingMutation.mutate({ kind: 'track', itemId: trackId, rating: picked })}
		>
			<span class="relative inline-flex size-6 items-center justify-center">
				<Star class="size-6 {stars > 0 ? 'fill-warning text-warning' : ''}" aria-hidden="true" />
				{#if stars > 0}
					<span class="absolute text-[10px] font-bold leading-none text-warning-content"
						>{stars}</span
					>
				{/if}
			</span>
		</StarPicker>
	</div>
{/if}
