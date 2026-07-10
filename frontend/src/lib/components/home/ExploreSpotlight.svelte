<script lang="ts">
	/*
	 * ExploreSpotlight — the cinematic full-bleed masthead of the new Home.
	 * Rotates through the week's top albums (real chart data); the primary CTA
	 * starts a real radio station seeded from the spotlighted artist, the
	 * secondary opens the discography. Dots + arrows cycle the spotlight.
	 */
	import { getApiUrl } from '$lib/api/api-utils';
	import { formatListenCount } from '$lib/utils/formatting';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { fromStore } from 'svelte/store';
	import type { HomeAlbum } from '$lib/types';
	import { ChevronLeft, ChevronRight, Disc3 } from 'lucide-svelte';

	interface Props {
		items: HomeAlbum[];
		eyebrow?: string;
	}

	let { items, eyebrow = 'Artist spotlight' }: Props = $props();

	const integrations = fromStore(integrationStore);

	let index = $state(0);
	let coverFailed = $state<Record<string, boolean>>({});

	const spotlight = $derived(items[Math.min(index, Math.max(0, items.length - 1))]);

	function coverUrl(mbid: string): string {
		return getApiUrl(`/api/v1/covers/release-group/${mbid}?size=500`);
	}

	function next(): void {
		index = (index + 1) % items.length;
	}
	function prev(): void {
		index = (index - 1 + items.length) % items.length;
	}

	// "GOLDEN" → lead "GOLDEN"; "Map of the Soul" → lead "Map of the" + italic "Soul"
	const titleParts = $derived.by(() => {
		const words = (spotlight?.name ?? '').split(' ');
		if (words.length < 2) return { lead: spotlight?.name ?? '', accent: '' };
		return { lead: words.slice(0, -1).join(' '), accent: words[words.length - 1] };
	});
</script>

{#if spotlight}
	<section class="dn-spotlight" aria-label="Artist spotlight">
		<div class="dn-spotlight__backdrop" aria-hidden="true">
			{#key spotlight.mbid ?? spotlight.name}
				{#if spotlight.mbid && !coverFailed[spotlight.mbid]}
					<img
						src={coverUrl(spotlight.mbid)}
						alt=""
						onerror={() => (coverFailed = { ...coverFailed, [spotlight.mbid ?? '']: true })}
					/>
				{/if}
			{/key}
			<div class="dn-spotlight__wash"></div>
		</div>

		<div class="dn-spotlight__content">
			<p class="dn-spotlight__eyebrow"><span>{eyebrow}</span></p>

			<h1 class="dn-spotlight__title hero-title">
				{titleParts.lead}
				{#if titleParts.accent}<em>{titleParts.accent}</em>{/if}
			</h1>

			<p class="dn-spotlight__meta">
				{spotlight.artist_name}
				{#if spotlight.listen_count}
					· {formatListenCount(spotlight.listen_count)} this week
				{/if}
				{#if spotlight.in_library}
					· <span class="text-accent">In your collection</span>
				{/if}
			</p>

			<div class="mt-6 flex flex-wrap items-center gap-3">
				{#if spotlight.artist_mbid}
					<RadioPlayButton
						seed={{ seed_type: 'artist', seed_id: spotlight.artist_mbid }}
						forcePreviews={!integrations.current.loaded ||
							(!integrations.current.youtube && !integrations.current.localfiles)}
						size="md"
						variant="primary"
						label="Spin station"
					/>
					<a
						href="/artist/{spotlight.artist_mbid}"
						class="btn btn-ghost gap-2 rounded-full bg-base-100/40"
					>
						<Disc3 class="h-4 w-4" />
						View discography
					</a>
				{:else if spotlight.mbid}
					<a href="/album/{spotlight.mbid}" class="btn btn-primary gap-2 rounded-full">
						<Disc3 class="h-4 w-4" />
						Open album
					</a>
				{/if}
			</div>
		</div>

		{#if items.length > 1}
			<div class="dn-spotlight__pager">
				<button
					class="btn btn-ghost btn-xs btn-circle"
					onclick={prev}
					aria-label="Previous spotlight"
				>
					<ChevronLeft class="h-4 w-4" />
				</button>
				<div class="flex items-center gap-1.5" role="tablist" aria-label="Spotlight items">
					{#each items as item, i (item.mbid ?? `${item.name}-${i}`)}
						<button
							class="dn-spotlight__dot"
							class:dn-spotlight__dot--active={i === index}
							onclick={() => (index = i)}
							aria-label="Spotlight {i + 1}: {item.name}"
							aria-current={i === index}
						></button>
					{/each}
				</div>
				<button class="btn btn-ghost btn-xs btn-circle" onclick={next} aria-label="Next spotlight">
					<ChevronRight class="h-4 w-4" />
				</button>
			</div>
		{/if}
	</section>
{/if}

<style>
	.dn-spotlight {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		min-height: min(62vh, 34rem);
		display: flex;
		flex-direction: column;
		justify-content: center;
		padding: 4rem 1.25rem 4.5rem;
	}
	@media (min-width: 640px) {
		.dn-spotlight {
			padding: 5rem 2rem 5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-spotlight {
			padding: 6rem 3rem 5.5rem;
		}
	}

	.dn-spotlight__backdrop {
		position: absolute;
		inset: 0;
		z-index: -1;
	}
	.dn-spotlight__backdrop img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		object-position: center 30%;
		filter: saturate(0.9) brightness(0.72);
		transition: opacity var(--dn-dur-slow) ease;
	}
	.dn-spotlight__wash {
		position: absolute;
		inset: 0;
		background:
			linear-gradient(
				90deg,
				oklch(from var(--color-base-100) l c h / 0.92) 0%,
				oklch(from var(--color-base-100) l c h / 0.55) 45%,
				oklch(from var(--color-base-100) l c h / 0.2) 100%
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-100) l c h / 0.35) 0%,
				oklch(from var(--color-base-100) l c h / 0.1) 40%,
				var(--color-base-100) 100%
			);
	}

	.dn-spotlight__eyebrow span {
		display: inline-block;
		padding: 0.32rem 0.85rem;
		border-radius: 999px;
		border: 1px solid var(--dn-hairline-strong);
		background: oklch(from var(--color-base-100) l c h / 0.45);
		backdrop-filter: blur(8px);
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.22em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.75);
	}

	.dn-spotlight__title {
		margin-top: 1rem;
		max-width: 16ch;
		font-family: var(--font-display);
		font-size: clamp(2.6rem, 6.5vw, 4.75rem);
		font-weight: 700;
		line-height: 0.98;
		letter-spacing: -0.02em;
		text-wrap: balance;
	}
	.dn-spotlight__title em {
		font-style: italic;
		font-weight: 500;
	}

	.dn-spotlight__meta {
		margin-top: 1rem;
		max-width: 40rem;
		font-size: 1rem;
		color: oklch(from var(--color-base-content) l c h / 0.72);
	}

	.dn-spotlight__pager {
		position: absolute;
		right: 1.25rem;
		bottom: 1.1rem;
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
	.dn-spotlight__dot {
		width: 0.45rem;
		height: 0.45rem;
		border-radius: 999px;
		background: oklch(from var(--color-base-content) l c h / 0.25);
		transition:
			background var(--dn-dur-fast) ease,
			transform var(--dn-dur-fast) ease;
	}
	.dn-spotlight__dot--active {
		background: oklch(from var(--color-primary) l c h / 0.95);
		transform: scale(1.25);
	}
</style>
