<!--
	A page's section strip: one link per section, an acid underline on the
	one in view, held under the top bar while the page scrolls. The links
	jump (they are not tabs that swap content). On a narrow screen the strip
	scrolls sideways and keeps the current link in view. A section's own
	`scroll-margin-top` should clear the top bar and this strip
	(`scroll-mt-32`).
-->
<script lang="ts" module>
	export interface JumpSection {
		id: string;
		label: string;
	}
</script>

<script lang="ts">
	import { browser } from '$app/environment';

	interface Props {
		sections: JumpSection[];
		/** The strip's name, for screen readers. */
		label?: string;
		/** Layout only: margin. */
		class?: string;
	}

	let { sections, label = 'Page sections', class: className = '' }: Props = $props();

	let current = $state('');
	let strip = $state<HTMLElement | null>(null);

	// The section whose top has passed the strip, else the first.
	function inView(): string {
		if (!browser || sections.length === 0) return '';
		const line = 160;
		let found = sections[0].id;
		for (const section of sections) {
			const element = document.getElementById(section.id);
			if (!element) continue;
			if (element.getBoundingClientRect().top - line <= 0) found = section.id;
			else break;
		}
		return found;
	}

	$effect(() => {
		// eslint-disable-next-line @typescript-eslint/no-unused-expressions
		sections;
		if (!browser) return;
		let frame = 0;
		const update = () => {
			frame = 0;
			current = inView();
		};
		const schedule = () => {
			if (!frame) frame = requestAnimationFrame(update);
		};
		const timer = window.setTimeout(update, 0);
		window.addEventListener('scroll', schedule, { passive: true });
		window.addEventListener('resize', schedule);
		return () => {
			window.clearTimeout(timer);
			if (frame) cancelAnimationFrame(frame);
			window.removeEventListener('scroll', schedule);
			window.removeEventListener('resize', schedule);
		};
	});

	// Keep the current link in view when the strip scrolls sideways.
	$effect(() => {
		if (!strip || !current) return;
		const link = strip.querySelector<HTMLElement>(`[data-section="${CSS.escape(current)}"]`);
		if (!link) return;
		const left = link.offsetLeft - strip.clientWidth / 2 + link.clientWidth / 2;
		strip.scrollTo({ left, behavior: 'auto' });
	});

	function jump(event: MouseEvent, id: string) {
		const element = document.getElementById(id);
		if (!element) return;
		event.preventDefault();
		const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
		element.scrollIntoView({ behavior: still ? 'auto' : 'smooth', block: 'start' });
		history.replaceState(history.state, '', `#${id}`);
		current = id;
	}
</script>

{#if sections.length > 1}
	<nav class="dn-jump {className}" aria-label={label}>
		<ul class="dn-jump__list" bind:this={strip}>
			{#each sections as section (section.id)}
				<li>
					<a
						href={`#${section.id}`}
						data-section={section.id}
						class="dn-jump__link"
						aria-current={current === section.id ? 'true' : undefined}
						onclick={(event) => jump(event, section.id)}>{section.label}</a
					>
				</li>
			{/each}
		</ul>
	</nav>
{/if}

<style>
	.dn-jump {
		position: sticky;
		top: calc(4rem + var(--ms-safe-top));
		z-index: 40;
		background-color: var(--color-base-100);
		border-bottom: 1px solid var(--dn-line);
	}
	@media (min-width: 768px) {
		.dn-jump {
			top: 4rem;
		}
		.dn-jump__list {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-jump__list {
			padding-inline: 2rem;
		}
	}

	/* the links line up with the page's 80rem column */
	.dn-jump__list {
		display: flex;
		gap: 0.25rem;
		max-width: 80rem;
		margin: 0 auto;
		padding-inline: 1rem;
		overflow-x: auto;
		scrollbar-width: none;
		overscroll-behavior-x: contain;
	}
	.dn-jump__list::-webkit-scrollbar {
		display: none;
	}

	.dn-jump__link {
		position: relative;
		display: block;
		padding: 0.875rem 1rem 0.8125rem;
		font-size: 0.9375rem;
		font-weight: 600;
		white-space: nowrap;
		color: var(--dn-muted);
		transition: color 150ms;
	}
	.dn-jump__link:hover {
		color: var(--color-base-content);
	}
	.dn-jump__link:focus-visible {
		outline-offset: -2px;
	}
	.dn-jump__link[aria-current='true'] {
		color: var(--dn-title);
	}
	.dn-jump__link[aria-current='true']::after {
		content: '';
		position: absolute;
		inset: auto 0 -1px;
		height: 3px;
		background-color: var(--dn-title);
	}
	li:first-child .dn-jump__link {
		padding-left: 0.25rem;
	}
	li:first-child .dn-jump__link[aria-current='true']::after {
		left: 0;
	}

	@media (forced-colors: active) {
		.dn-jump__link[aria-current='true'] {
			text-decoration: underline;
		}
	}
</style>
