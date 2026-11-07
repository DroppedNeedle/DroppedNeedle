<!--
	The round artist faces shared by the Library's Artists tile and the Home
	Discover teaser: three at once, the front one largest, turning with the
	caller's `cycle` (a deckBeat count, plus an offset to stagger it from a
	deck beside it). The caller owns the box and sets container-type: size;
	the faces are sized in cqh so the same fan works in both places.
-->
<script lang="ts">
	import ArtistImage from './ArtistImage.svelte';
	import { deckSlots } from './deckBeat.svelte';

	interface ArtistFanItem {
		key: string;
		mbid: string;
		available?: boolean;
	}

	interface Props {
		artists: ArtistFanItem[];
		cycle: number;
		/** Root class for the caller's box, width and position. */
		class?: string;
	}

	let { artists, cycle, class: className = '' }: Props = $props();

	const WINDOW = 3;
</script>

<span class="artist-fan {className}" aria-hidden="true">
	{#each deckSlots(artists, cycle, WINDOW) as slot (slot.item.key)}
		<!-- with fewer faces than the window, they take the front slots -->
		<span
			class="artist-fan__face"
			data-slot={slot.position + WINDOW - Math.min(WINDOW, artists.length)}
			data-fan-key={slot.item.key}
		>
			<ArtistImage
				mbid={slot.item.mbid}
				available={slot.item.available}
				alt=""
				size="full"
				requestSize={250}
				rounded="none"
				className="h-full w-full object-cover"
			/>
		</span>
	{/each}
</span>

<style>
	.artist-fan {
		position: absolute;
	}

	.artist-fan__face {
		position: absolute;
		overflow: hidden;
		border-radius: 9999px;
		box-shadow:
			0 0 0 3px var(--color-base-100),
			0 10px 22px -8px rgb(0 0 0 / 0.7);
		transition:
			top 0.7s cubic-bezier(0.22, 1, 0.36, 1),
			right 0.7s cubic-bezier(0.22, 1, 0.36, 1),
			width 0.7s cubic-bezier(0.22, 1, 0.36, 1),
			height 0.7s cubic-bezier(0.22, 1, 0.36, 1);
	}
	.artist-fan__face[data-slot='0'] {
		z-index: 1;
		top: 12%;
		right: 27cqh;
		width: 45cqh;
		height: 45cqh;
	}
	.artist-fan__face[data-slot='1'] {
		z-index: 2;
		top: 43%;
		right: 45cqh;
		width: 34cqh;
		height: 34cqh;
	}
	.artist-fan__face[data-slot='2'] {
		z-index: 3;
		top: 32%;
		right: 2cqh;
		width: 64cqh;
		height: 64cqh;
	}

	@media (prefers-reduced-motion: reduce) {
		.artist-fan__face {
			transition: none;
		}
	}
</style>
