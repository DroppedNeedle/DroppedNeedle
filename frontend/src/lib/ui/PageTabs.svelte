<!--
	A strip of links between a set of sibling pages, the same on each of
	them: big labels with an icon and an optional count, the current page a
	filled acid tab marked `aria-current="page"`. Unlike JumpTabs the links
	open pages, not sections. On a narrow screen the strip scrolls sideways
	and starts with the current tab in view; on a phone it runs out over
	the page's 1rem gutters to the screen's edges.
-->
<script lang="ts" module>
	import type { KitIcon } from './Button.svelte';

	export interface PageTab {
		id: string;
		label: string;
		/** The page's address, base path included. */
		href: string;
		icon?: KitIcon;
		/** A count after the label; nothing when absent. */
		count?: number | null;
	}
</script>

<script lang="ts">
	interface Props {
		tabs: readonly PageTab[];
		/** The id of the page being shown. */
		current: string;
		/** The strip's name, for screen readers. */
		label: string;
		/** Layout only: margin. */
		class?: string;
	}

	let { tabs, current, label, class: className = '' }: Props = $props();

	let list = $state<HTMLUListElement | null>(null);

	// Centre the current tab in the strip when the strip is wider than the
	// screen, scrolling only the strip, never the page. It runs again when
	// any tab changes size (a web font arriving, a count), since any of them
	// can move the current one.
	$effect(() => {
		if (!list) return;
		const strip = list;
		const link = strip.querySelector<HTMLElement>(`[data-tab="${CSS.escape(current)}"]`);
		if (!link) return;
		const centre = () => {
			if (strip.scrollWidth <= strip.clientWidth) return;
			// offsets within the strip (it is the tab's offset parent), so the
			// answer does not depend on where the page or strip is scrolled
			strip.scrollLeft = link.offsetLeft - (strip.clientWidth - link.offsetWidth) / 2;
		};
		centre();
		const watch = new ResizeObserver(centre);
		watch.observe(strip);
		for (const tab of strip.querySelectorAll('.dn-page-tabs__link')) watch.observe(tab);
		return () => watch.disconnect();
	});
</script>

<nav class="dn-page-tabs {className}" aria-label={label}>
	<ul class="dn-page-tabs__list" bind:this={list}>
		{#each tabs as tab (tab.id)}
			{@const Icon = tab.icon}
			<li>
				<a
					href={tab.href}
					class="dn-page-tabs__link"
					data-tab={tab.id}
					aria-current={tab.id === current ? 'page' : undefined}
				>
					{#if Icon}<Icon class="dn-page-tabs__icon" aria-hidden="true" />{/if}
					<span>{tab.label}</span>
					{#if tab.count != null}
						<span class="dn-page-tabs__count">{tab.count}</span>
					{/if}
				</a>
			</li>
		{/each}
	</ul>
</nav>

<style>
	.dn-page-tabs {
		max-width: 100%;
	}
	.dn-page-tabs__list {
		position: relative;
		display: flex;
		gap: 0.25rem;
		width: fit-content;
		max-width: 100%;
		padding: 0.3125rem;
		overflow-x: auto;
		border: 1px solid var(--dn-edge);
		border-radius: 0.75rem;
		background-color: var(--color-base-200);
		scrollbar-width: none;
		overscroll-behavior-x: contain;
	}
	.dn-page-tabs__list::-webkit-scrollbar {
		display: none;
	}
	.dn-page-tabs__list > li {
		flex: none;
	}
	.dn-page-tabs__link {
		display: inline-flex;
		align-items: center;
		gap: 0.5625rem;
		padding: 0.625rem 1.125rem;
		border-radius: 0.5rem;
		font-size: 1rem;
		font-weight: 600;
		line-height: 1.25;
		white-space: nowrap;
		color: var(--color-base-content);
		transition: background-color 150ms;
	}
	.dn-page-tabs__link:hover {
		background-color: color-mix(in oklab, var(--color-base-content) 8%, transparent);
	}
	.dn-page-tabs__link:focus-visible {
		outline-offset: -2px;
	}
	.dn-page-tabs__link[aria-current='page'] {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
	.dn-page-tabs :global(.dn-page-tabs__icon) {
		flex: none;
		width: 1.125rem;
		height: 1.125rem;
	}
	.dn-page-tabs__count {
		padding: 0.125rem 0.5rem;
		border-radius: 9999px;
		background-color: color-mix(in oklab, var(--color-base-content) 12%, transparent);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		line-height: 1.3;
	}

	/* a phone: the strip runs across the page's 1rem gutters to the screen's
	   edges, and each tab is outlined */
	@media (max-width: 639px) {
		.dn-page-tabs__list {
			width: auto;
			max-width: none;
			margin-inline: -1rem;
			padding: 0.25rem 1rem;
			scroll-padding-inline: 1rem;
			border: 0;
			border-radius: 0;
			background-color: transparent;
		}
		.dn-page-tabs__link {
			gap: 0.5rem;
			padding: 0.5625rem 1rem;
			font-size: 0.9375rem;
			box-shadow: inset 0 0 0 1px var(--dn-edge);
		}
	}

	@media (forced-colors: active) {
		.dn-page-tabs__link[aria-current='page'] {
			text-decoration: underline;
		}
	}
</style>
