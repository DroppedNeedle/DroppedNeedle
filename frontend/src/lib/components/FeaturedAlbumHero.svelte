<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { Play } from 'lucide-svelte';

	interface AlbumSummary {
		name: string;
		artist_name: string;
		image_url?: string | null;
		year?: number | null;
		musicbrainz_id?: string | null;
		[key: string]: unknown;
	}

	interface Props {
		albums: AlbumSummary[];
		idKey: string;
		onAlbumClick?: (album: AlbumSummary) => void;
	}

	let { albums, idKey, onAlbumClick }: Props = $props();

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

	function getId(album: AlbumSummary): string {
		return String(album[idKey] ?? album.name);
	}
</script>

{#if hero}
	<canvas bind:this={canvasEl} class="hidden" width="1" height="1"></canvas>
	<section class="dn-featured animate-fade-in-up" aria-label="Continue listening">
		<div class="dn-featured__backdrop" aria-hidden="true">
			{#if hero.image_url}
				<img src={hero.image_url} alt="" crossorigin="anonymous" onload={handleHeroLoad} />
			{/if}
			{#if glowColor}
				<div
					class="absolute inset-0"
					style="background: radial-gradient(ellipse at 28% 85%, rgba({glowColor}, 0.22), transparent 70%);"
				></div>
			{/if}
			<div class="dn-featured__wash"></div>
		</div>

		<div
			class="relative z-10 flex min-h-[220px] flex-col justify-center gap-6 px-6 py-8 sm:min-h-[260px] sm:flex-row sm:items-center sm:justify-start sm:gap-8 sm:px-8 sm:py-10"
		>
			<button
				class="dn-featured__cover self-start sm:self-auto"
				aria-label="Play {hero.name} by {hero.artist_name}"
				onclick={() => onAlbumClick?.(hero)}
			>
				<div class="h-[130px] w-[130px] sm:h-[160px] sm:w-[160px]">
					<AlbumImage
						mbid={hero.musicbrainz_id ?? getId(hero)}
						customUrl={hero.image_url}
						alt={hero.name}
						size="full"
						rounded="none"
						className="h-full w-full"
					/>
				</div>
			</button>

			<div class="min-w-0">
				<p class="dn-featured__eyebrow"><span>Continue Listening</span></p>
				<h3
					class="mt-3 line-clamp-2 font-display text-2xl font-bold tracking-tight text-base-content sm:text-4xl"
				>
					{hero.name}
				</h3>
				<p class="mt-1.5 line-clamp-1 text-sm text-base-content/65">
					{hero.artist_name}{#if hero.year}<span class="text-base-content/45">
							· {hero.year}</span
						>{/if}
				</p>
				<div class="mt-5">
					<button
						class="btn gap-2 rounded-full border-0 bg-base-content text-base-100 shadow-[0_10px_30px_rgba(0,0,0,0.35)] hover:bg-base-content/85"
						onclick={() => onAlbumClick?.(hero)}
					>
						<Play class="h-4 w-4 fill-current" />
						Play Now
					</button>
				</div>
			</div>
		</div>

		{#if thumbnails.length > 0}
			<div class="relative z-10 flex gap-3 overflow-x-auto px-6 pb-6 scrollbar-hide sm:px-8">
				{#each thumbnails as album (getId(album))}
					<button
						class="dn-featured__thumb"
						aria-label="Play {album.name}"
						onclick={() => onAlbumClick?.(album)}
					>
						<div class="h-[68px] w-[68px] sm:h-[76px] sm:w-[76px]">
							<AlbumImage
								mbid={album.musicbrainz_id ?? getId(album)}
								customUrl={album.image_url}
								alt={album.name}
								size="full"
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

<style>
	.dn-featured {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		border-radius: 1rem;
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.08);
	}

	.dn-featured__backdrop {
		position: absolute;
		inset: 0;
		z-index: -1;
		pointer-events: none;
	}
	.dn-featured__backdrop img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		object-position: center 35%;
		transform: scale(1.15);
		filter: blur(28px) saturate(0.9) brightness(0.6);
	}
	.dn-featured__wash {
		position: absolute;
		inset: 0;
		background:
			linear-gradient(
				90deg,
				oklch(from var(--color-base-100) l c h / 0.88) 0%,
				oklch(from var(--color-base-100) l c h / 0.55) 45%,
				oklch(from var(--color-base-100) l c h / 0.25) 100%
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-100) l c h / 0.25) 0%,
				oklch(from var(--color-base-100) l c h / 0.1) 45%,
				oklch(from var(--color-base-100) l c h / 0.85) 100%
			);
	}

	.dn-featured__eyebrow span {
		display: inline-block;
		padding: 0.32rem 0.85rem;
		border-radius: 999px;
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.15);
		background: oklch(from var(--color-base-100) l c h / 0.45);
		backdrop-filter: blur(8px);
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.22em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.75);
	}

	.dn-featured__cover {
		flex-shrink: 0;
		overflow: hidden;
		border-radius: 0.75rem;
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.12);
		box-shadow: 0 20px 60px rgba(0, 0, 0, 0.5);
		transform: perspective(600px) rotateY(-2deg);
		transform-style: preserve-3d;
		transition: transform 0.5s ease;
	}
	.dn-featured__cover:hover {
		transform: perspective(600px) rotateY(0deg) scale(1.03);
	}

	.dn-featured__thumb {
		flex-shrink: 0;
		overflow: hidden;
		border-radius: 0.65rem;
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.1);
		transition:
			transform 0.3s var(--ease-overshoot, ease),
			border-color 0.3s ease;
	}
	.dn-featured__thumb:hover,
	.dn-featured__thumb:focus-visible {
		transform: scale(1.05);
		border-color: oklch(from var(--color-primary) l c h / 0.6);
		outline: none;
	}
</style>
