<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy } from 'svelte';
	import { List } from 'lucide-svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { SvelteMap } from 'svelte/reactivity';

	type PageSection = {
		id: string;
		label: string;
	};

	interface Props {
		sections: PageSection[];
		className?: string;
	}

	let { sections, className = '' }: Props = $props();

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
		sections;
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
	<aside class="hidden xl:block {className}">
		<nav class="sticky top-24" aria-label="Page sections">
			<p class="mb-2 pl-3 text-[10px] font-semibold uppercase tracking-widest text-base-content/40">
				On this page
			</p>
			<ul class="flex flex-col border-l border-base-content/10">
				{#each sections as section (section.id)}
					<li>
						<a
							href={`#${section.id}`}
							onclick={(event) => scrollToSection(event, section.id)}
							class="-ml-px block border-l-2 py-1.5 pl-3 text-xs transition-colors duration-150
								{activeSectionId === section.id
								? 'border-primary text-primary font-semibold'
								: 'border-transparent text-base-content/50 hover:text-base-content/80 hover:border-base-content/30'}"
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
		class="fixed left-4 z-40 xl:hidden"
		class:bottom-36={playerStore.isPlayerVisible}
		class:bottom-6={!playerStore.isPlayerVisible}
	>
		<details bind:this={mobileMenu} class="dropdown dropdown-top dropdown-start">
			<summary class="btn btn-circle btn-primary shadow-lg" aria-label="Open section navigation">
				<List class="h-5 w-5" />
			</summary>
			<div
				class="dropdown-content z-[1] mb-2 w-52 rounded-box border border-base-content/10 bg-base-200/95 p-3 shadow-xl backdrop-blur-md"
			>
				<p class="mb-2 text-[10px] font-semibold uppercase tracking-widest text-base-content/40">
					Jump to
				</p>
				<ul class="flex flex-col border-l border-base-content/10">
					{#each sections as section (section.id)}
						<li>
							<a
								href={`#${section.id}`}
								onclick={(event) => scrollToSection(event, section.id)}
								class="-ml-px block border-l-2 py-1.5 pl-3 text-sm transition-colors duration-150
									{activeSectionId === section.id
									? 'border-primary text-primary font-semibold'
									: 'border-transparent text-base-content/60 hover:text-base-content hover:border-base-content/30'}"
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
