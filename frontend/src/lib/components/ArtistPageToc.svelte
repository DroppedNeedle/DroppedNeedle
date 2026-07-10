<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy } from 'svelte';
	import { List } from 'lucide-svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { SvelteMap } from 'svelte/reactivity';

	type ArtistPageTocSection = {
		id: string;
		label: string;
	};

	interface Props {
		sections: ArtistPageTocSection[];
	}

	let { sections }: Props = $props();

	let activeSectionId = $state('');
	let observer: IntersectionObserver | null = null;
	let sectionRatios = new SvelteMap<string, number>();
	let mobileMenu = $state<HTMLDetailsElement | null>(null);

	function getFallbackActiveSection(): string {
		if (!browser || sections.length === 0) return '';

		const offset = 140;
		let fallback = sections[0].id;

		for (const section of sections) {
			const element = document.getElementById(section.id);
			if (!element) continue;
			if (element.getBoundingClientRect().top - offset <= 0) {
				fallback = section.id;
			} else {
				break;
			}
		}

		return fallback;
	}

	function recalculateActiveSection() {
		const candidates = sections
			.map((section) => ({
				id: section.id,
				ratio: sectionRatios.get(section.id) ?? 0
			}))
			.filter((candidate) => candidate.ratio > 0);

		if (candidates.length > 0) {
			candidates.sort((a, b) => b.ratio - a.ratio);
			activeSectionId = candidates[0].id;
			return;
		}

		activeSectionId = getFallbackActiveSection();
	}

	function setupObserver() {
		if (!browser) return;

		observer?.disconnect();
		observer = null;

		if (sections.length === 0) {
			activeSectionId = '';
			return;
		}

		observer = new IntersectionObserver(
			(entries) => {
				for (const entry of entries) {
					sectionRatios.set(
						(entry.target as HTMLElement).id,
						entry.isIntersecting ? entry.intersectionRatio : 0
					);
				}
				recalculateActiveSection();
			},
			{
				rootMargin: '-20% 0px -70% 0px',
				threshold: [0, 0.25, 0.5, 0.75, 1]
			}
		);

		for (const section of sections) {
			const element = document.getElementById(section.id);
			if (element) {
				observer.observe(element);
			}
		}

		activeSectionId = getFallbackActiveSection();
	}

	function scrollToSection(event: Event, id: string) {
		event.preventDefault();
		const element = document.getElementById(id);
		if (!element) return;
		element.scrollIntoView({ behavior: 'smooth', block: 'start' });
		activeSectionId = id;
		if (mobileMenu?.open) {
			mobileMenu.open = false;
		}
	}

	$effect(() => {
		// eslint-disable-next-line @typescript-eslint/no-unused-expressions
		sections; // Track reactivity
		if (!browser) return;

		const timeoutId = window.setTimeout(setupObserver, 0);
		return () => {
			window.clearTimeout(timeoutId);
		};
	});

	onDestroy(() => {
		observer?.disconnect();
		observer = null;
	});

	const showToc = $derived(sections.length > 1);
</script>

{#if showToc}
	<aside class="hidden xl:block">
		<nav class="sticky top-24" aria-label="Page sections">
			<p
				class="mb-3 pl-1 font-mono text-[0.6rem] font-bold uppercase tracking-[0.2em] text-base-content/40"
			>
				On this page
			</p>
			<ul class="flex flex-col items-start gap-1.5">
				{#each sections as section (section.id)}
					<li>
						<a
							href={`#${section.id}`}
							onclick={(event) => scrollToSection(event, section.id)}
							class="btn btn-xs rounded-full font-medium normal-case
								{activeSectionId === section.id
								? 'btn-primary'
								: 'btn-ghost border border-base-content/10 bg-base-200/40 text-base-content/60 hover:border-base-content/25 hover:text-base-content'}"
							aria-current={activeSectionId === section.id ? 'true' : undefined}
						>
							{section.label}
						</a>
					</li>
				{/each}
			</ul>
		</nav>
	</aside>

	<div
		class="xl:hidden fixed left-4 z-40"
		class:bottom-36={playerStore.isPlayerVisible}
		class:bottom-6={!playerStore.isPlayerVisible}
	>
		<details bind:this={mobileMenu} class="dropdown dropdown-top dropdown-start">
			<summary class="btn btn-circle btn-primary shadow-lg" aria-label="Open section navigation">
				<List class="h-5 w-5" />
			</summary>
			<div
				class="dropdown-content z-[1] mb-2 w-56 rounded-2xl border border-base-content/10 bg-base-200/95 p-3 shadow-xl backdrop-blur-md"
			>
				<p
					class="mb-2.5 font-mono text-[0.6rem] font-bold uppercase tracking-[0.2em] text-base-content/40"
				>
					Jump to
				</p>
				<ul class="flex flex-wrap items-start gap-1.5">
					{#each sections as section (section.id)}
						<li>
							<a
								href={`#${section.id}`}
								onclick={(event) => scrollToSection(event, section.id)}
								class="btn btn-xs rounded-full font-medium normal-case
									{activeSectionId === section.id
									? 'btn-primary'
									: 'btn-ghost border border-base-content/10 bg-base-100/60 text-base-content/70 hover:border-base-content/25 hover:text-base-content'}"
								aria-current={activeSectionId === section.id ? 'true' : undefined}
							>
								{section.label}
							</a>
						</li>
					{/each}
				</ul>
			</div>
		</details>
	</div>
{/if}
