<!--
	The heart and stars of the track playing, in the player bar and on the
	Listening Room's deck. A track from a media server rates its library
	copy; one with no library copy shows nothing. `stars={false}` shows the
	heart alone.
-->
<script lang="ts">
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { libraryTrackId } from '$lib/features/player/state/skipByRating.svelte';
	import { getTrackFavoriteQuery } from '$lib/features/player/queries/favoriteTrack.svelte';
	import FavoriteHeart from '$lib/features/library/components/FavoriteHeart.svelte';
	import { createSetFavoriteMutation } from '$lib/features/library/queries/FavoriteMutations.svelte';
	import {
		createSetRatingMutation,
		getRatingsQuery,
		ratingOf
	} from '$lib/features/library/queries/ratings.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';

	interface Props {
		/** The stars beside the heart, from the large width up. */
		stars?: boolean;
		/** The stars at every width, not only from the large width up. */
		alwaysStars?: boolean;
		class?: string;
	}

	let { stars = true, alwaysStars = false, class: className = '' }: Props = $props();

	const trackId = $derived.by(() => {
		const np = playerStore.nowPlaying;
		const item = playerStore.currentQueueItem;
		if (item && item.trackSourceId === np?.trackSourceId) return libraryTrackId(item);
		return np?.sourceType === 'local' ? (np.trackSourceId ?? null) : null;
	});
	const title = $derived(playerStore.nowPlaying?.trackName ?? 'This track');

	const favoriteQuery = getTrackFavoriteQuery(() => trackId);
	const ratingsQuery = getRatingsQuery(() => (trackId && stars ? { trackIds: [trackId] } : null));
	const heart = createSetFavoriteMutation();
	const rate = createSetRatingMutation();

	// A heart given here shows at once, until the favourites list is read again.
	let given = $state<{ id: string; favorite: boolean; readAt: number } | null>(null);
	const favorite = $derived(
		given && given.id === trackId && given.readAt === favoriteQuery.readAt
			? given.favorite
			: favoriteQuery.favorite
	);

	function setFavorite(id: string, favorited: boolean): void {
		given = { id, favorite: favorited, readAt: favoriteQuery.readAt };
		heart.mutate({ kind: 'track', itemId: id, favorited }, { onError: () => (given = null) });
	}
	const rating = $derived(trackId ? ratingOf(ratingsQuery.data, 'track', trackId) : 0);
</script>

{#if trackId}
	{@const id = trackId}
	<span class="inline-flex items-center {className}">
		<FavoriteHeart {favorite} onchange={(favorited) => setFavorite(id, favorited)} />
		{#if stars}
			<RatingStars
				class={alwaysStars ? 'inline-flex' : 'hidden lg:inline-flex'}
				quiet
				label={title}
				value={rating}
				onchange={(value) => rate.mutate({ kind: 'track', itemId: id, rating: value })}
			/>
		{/if}
	</span>
{/if}
