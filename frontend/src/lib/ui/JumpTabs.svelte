<!--
	A page's section strip: one link per section, an acid underline on the
	one in view, held under the top bar while the page scrolls. The links
	jump (they are not tabs that swap content). On a narrow screen the strip
	scrolls sideways and keeps the current link in view. A section's own
	`scroll-margin-top` should clear the top bar and this strip
	(`scroll-mt-32`). `pills` marks the current link as a filled acid pill
	in place of the underline; `actions` holds a page's own controls at the
	strip's right end (they stay in view while the links scroll). `rail` is
	the same list standing in a page's left column: the label shown over it,
	a section's icon beside its name, the current one an acid pill, held in
	view below the top bar. `onjump` runs before the page scrolls, so a
	page can open a collapsed section first.
-->
<script lang="ts" module>
	import type { KitIcon } from './Button.svelte';

	export interface JumpSection {
		id: string;
		label: string;
		/** Shown beside the name in the rail. */
		icon?: KitIcon;
	}
</script>

<script lang="ts">
	import { tick, type Snippet } from 'svelte';
	import { browser } from '$app/environment';

	interface Props {
		sections: JumpSection[];
		/** The strip's name, for screen readers. */
		label?: string;
		/** How the current link is marked; `rail` is a vertical list. */
		look?: 'underline' | 'pills' | 'rail';
		/** Controls at the right end of the strip. */
		actions?: Snippet;
		/** Runs before the jump, with the section's id. */
		onjump?: (id: string) => void;
		/** Layout only: margin. */
		class?: string;
	}

	let {
		sections,
		label = 'Page sections',
		look = 'underline',
		actions,
		onjump,
		class: className = ''
	}: Props = $props();

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
		if (!strip || !current || look === 'rail') return;
		const link = strip.querySelector<HTMLElement>(`[data-section="${CSS.escape(current)}"]`);
		if (!link) return;
		const left = link.offsetLeft - strip.clientWidth / 2 + link.clientWidth / 2;
		strip.scrollTo({ left, behavior: 'auto' });
	});

	async function jump(event: MouseEvent, id: string) {
		if (!document.getElementById(id)) return;
		event.preventDefault();
		if (onjump) {
			onjump(id);
			await tick();
		}
		const element = document.getElementById(id);
		if (!element) return;
		const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
		element.scrollIntoView({ behavior: still ? 'auto' : 'smooth', block: 'start' });
		history.replaceState(history.state, '', `#${id}`);
		current = id;
	}
</script>

{#if sections.length > 1 || actions}
	<nav
		class="dn-jump {className}"
		class:dn-jump--pills={look === 'pills'}
		class:dn-jump--rail={look === 'rail'}
		aria-label={label}
	>
		{#if look === 'rail'}
			<p class="dn-jump__head" aria-hidden="true">{label}</p>
		{/if}
		<div class="dn-jump__row">
			<ul class="dn-jump__list" bind:this={strip}>
				{#each sections as section (section.id)}
					{@const Icon = section.icon}
					<li>
						<a
							href={`#${section.id}`}
							data-section={section.id}
							class="dn-jump__link"
							aria-current={current === section.id ? 'true' : undefined}
							onclick={(event) => void jump(event, section.id)}
						>
							{#if Icon && look === 'rail'}
								<Icon class="dn-jump__icon" aria-hidden="true" />
							{/if}
							{section.label}
						</a>
					</li>
				{/each}
			</ul>
			{#if actions}
				<div class="dn-jump__actions">{@render actions()}</div>
			{/if}
		</div>
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
		.dn-jump__row {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-jump__row {
			padding-inline: 2rem;
		}
	}

	/* the links line up with the page's 80rem column */
	.dn-jump__row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		max-width: 80rem;
		margin: 0 auto;
		padding-inline: 1rem;
	}
	.dn-jump__list {
		display: flex;
		flex: 1 1 auto;
		min-width: 0;
		gap: 0.25rem;
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

	.dn-jump__actions {
		display: flex;
		flex: none;
		align-items: center;
		gap: 0.25rem;
	}

	/* pills: the current link filled acid, the rest quiet */
	.dn-jump--pills .dn-jump__list {
		align-items: center;
		padding-block: 0.5rem;
	}
	.dn-jump--pills .dn-jump__link,
	.dn-jump--pills li:first-child .dn-jump__link {
		padding: 0.375rem 0.875rem;
		border-radius: 9999px;
		font-size: 0.875rem;
	}
	.dn-jump--pills .dn-jump__link[aria-current='true'] {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
	.dn-jump--pills .dn-jump__link[aria-current='true']::after {
		content: none;
	}

	/* rail: a vertical list in a page's left column */
	.dn-jump--rail {
		top: calc(4rem + 1.5rem);
		z-index: auto;
		background-color: transparent;
		border-bottom: 0;
	}
	.dn-jump__head {
		margin-bottom: 0.875rem;
		padding-left: 0.75rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.24em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.dn-jump--rail .dn-jump__row {
		display: block;
		max-width: none;
		padding-inline: 0;
	}
	.dn-jump--rail .dn-jump__list {
		flex-direction: column;
		gap: 0.125rem;
		overflow: visible;
	}
	.dn-jump--rail .dn-jump__link,
	.dn-jump--rail li:first-child .dn-jump__link {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.5rem 0.75rem;
		border-radius: 0.375rem;
		font-size: 0.875rem;
		white-space: normal;
		color: var(--color-base-content);
	}
	.dn-jump--rail .dn-jump__link:hover {
		background-color: color-mix(in oklab, var(--color-base-content) 8%, transparent);
	}
	.dn-jump--rail :global(.dn-jump__icon) {
		flex: none;
		width: 1.125rem;
		height: 1.125rem;
	}
	.dn-jump--rail .dn-jump__link[aria-current='true'] {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
	.dn-jump--rail .dn-jump__link[aria-current='true']::after {
		content: none;
	}

	@media (forced-colors: active) {
		.dn-jump__link[aria-current='true'] {
			text-decoration: underline;
		}
	}
</style>
