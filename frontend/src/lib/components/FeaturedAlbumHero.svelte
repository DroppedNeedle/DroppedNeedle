<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import type { RemoteAlbum } from '$lib/queries/remotes/types';

	interface Props {
		albums: RemoteAlbum[];
		onAlbumClick?: (album: RemoteAlbum) => void;
	}

	let { albums, onAlbumClick }: Props = $props();

	let hero = $derived(albums[0] ?? null);
	let thumbnails = $derived(albums.slice(1, 9));
	let glowColor = $state('');
	let canvasEl: HTMLCanvasElement | undefined = $state();

	function extractColor(imgEl: HTMLImageElement) {
		if (!canvasEl) return;
		try {
			const ctx = canvasEl.getContext('2d', { willReadFrequently: true });
			if (!ctx) return;
			canvasEl.width = 1;
			canvasEl.height = 1;
			ctx.drawImage(imgEl, 0, 0, 1, 1);
			const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data;
			glowColor = `${r}, ${g}, ${b}`;
		} catch {
			glowColor = '';
		}
	}

	function handleHeroLoad(e: Event) {
		const img = e.target as HTMLImageElement;
		if (img?.complete && img.naturalWidth > 0) {
			extractColor(img);
		}
	}
</script>

{#if hero}
	<canvas bind:this={canvasEl} class="hidden" width="1" height="1"></canvas>
	<section class="animate-fade-in-up space-y-0 overflow-hidden rounded-2xl">
		<div
			class="relative flex min-h-[220px] items-center gap-6 overflow-hidden px-6 py-6 sm:min-h-[260px] sm:px-8 sm:py-8"
		>
			{#if hero.image_url}
				<img
					src={getCoverUrl(hero.image_url, hero.release_group_mbid ?? hero.id)}
					alt=""
					aria-hidden="true"
					crossorigin="anonymous"
					class="pointer-events-none absolute inset-0 h-full w-full scale-110 object-cover blur-2xl brightness-[0.25]"
					onload={handleHeroLoad}
				/>
			{/if}

			{#if glowColor}
				<div
					class="pointer-events-none absolute inset-0"
					style="background: radial-gradient(ellipse at 30% 80%, rgba({glowColor}, 0.22), transparent 70%);"
				></div>
			{/if}

			<div
				class="pointer-events-none absolute inset-0 bg-gradient-to-r from-base-100/70 via-base-100/40 to-transparent"
			></div>

			<div class="relative z-10 flex items-center gap-6">
				<button
					class="shrink-0 overflow-hidden rounded-xl shadow-[0_20px_60px_rgba(0,0,0,0.5)] transition-transform duration-500 hover:scale-[1.03]"
					style:transform-style="preserve-3d"
					style:transform="perspective(600px) rotateY(-2deg)"
					aria-label="Play {hero.title} by {hero.artist_name}"
					onclick={() => onAlbumClick?.(hero)}
				>
					<div class="h-[120px] w-[120px] sm:h-[150px] sm:w-[150px]">
						<AlbumImage
							mbid={hero.release_group_mbid ?? hero.id}
							customUrl={hero.image_url}
							alt={hero.title}
							size="full"
							requestSize={250}
							rounded="none"
							priority
							className="h-full w-full"
						/>
					</div>
				</button>

				<div class="min-w-0 space-y-1">
					<p class="text-[10px] font-semibold uppercase tracking-[0.2em] text-base-content/50">
						Continue Listening
					</p>
					<h3 class="line-clamp-2 text-xl font-bold text-base-content sm:text-2xl">
						{hero.title}
					</h3>
					<p class="line-clamp-1 text-sm text-base-content/70">{hero.artist_name}</p>
					{#if hero.year}
						<span class="badge badge-sm badge-ghost mt-1">{hero.year}</span>
					{/if}
				</div>
			</div>
		</div>

		{#if thumbnails.length > 0}
			<div
				data-lazy-strip
				class="-mt-5 flex gap-3 overflow-x-auto px-6 pb-4 pt-5 scrollbar-hide [mask-image:linear-gradient(to_bottom,transparent,black_30%)]"
			>
				{#each thumbnails as album (album.id)}
					<button
						class="shrink-0 overflow-hidden rounded-lg ring-2 ring-base-100/30 transition-all duration-300 ease-[var(--ease-overshoot)] hover:scale-105 hover:ring-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"
						aria-label="Play {album.title}"
						onclick={() => onAlbumClick?.(album)}
					>
						<div class="h-[72px] w-[72px] sm:h-[80px] sm:w-[80px]">
							<AlbumImage
								mbid={album.release_group_mbid ?? album.id}
								customUrl={album.image_url}
								alt={album.title}
								size="full"
								requestSize={250}
								rounded="none"
								className="h-full w-full"
							/>
						</div>
					</button>
				{/each}
			</div>
		{/if}
	</section>
{/if}
