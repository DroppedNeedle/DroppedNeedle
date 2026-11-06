<!--
	The artist fan shared by the Library's Artists tile and the Home Discover
	teaser: three round faces, the front one largest, cycling through the
	source every 4.2s. The caller owns the box and sets container-type: size;
	the faces are sized in cqh so the same fan works in both places.
-->
<script lang="ts">
	import ArtistImage from './ArtistImage.svelte';

	interface ArtistFanItem {
		key: string;
		mbid: string;
		available?: boolean;
	}

	interface Props {
		artists: ArtistFanItem[];
		/** Faces shown at once. */
		windowSize?: number;
		/** Root class for the caller's box, width and position. */
		class?: string;
		/** Test ids for the Library tile's existing tests. */
		faceTestId?: string;
		imageTestId?: string;
	}

	let { artists, windowSize = 3, class: className = '', faceTestId, imageTestId }: Props = $props();

	let cycle = $state(0);
	let reduced = $state(false);

	$effect(() => {
		const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
		reduced = mq.matches;
		const onChange = (e: MediaQueryListEvent) => (reduced = e.matches);
		mq.addEventListener('change', onChange);
		return () => mq.removeEventListener('change', onChange);
	});

	$effect(() => {
		if (reduced) return;
		const id = setInterval(() => (cycle += 1), 4200);
		return () => clearInterval(id);
	});

	function visibleSlots(
		items: ArtistFanItem[],
		size: number
	): Array<{ item: ArtistFanItem; position: number }> {
		const n = items.length;
		const win = Math.min(size, n);
		const start = n <= size ? 0 : ((cycle % n) + n) % n;
		return Array.from({ length: win }, (_, position) => ({
			item: items[(start + position) % n]!,
			position
		}));
	}
</script>

<span class="artist-fan {className}" aria-hidden="true">
	{#each visibleSlots(artists, windowSize) as slot (slot.item.key)}
		<span
			class="artist-fan__face"
			data-slot={slot.position + windowSize - Math.min(windowSize, artists.length)}
			data-fan-key={slot.item.key}
			data-testid={faceTestId}
		>
			<ArtistImage
				mbid={slot.item.mbid}
				available={slot.item.available}
				alt=""
				size="full"
				requestSize={250}
				testId={imageTestId}
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
