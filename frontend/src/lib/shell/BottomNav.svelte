<!--
	The phone's bottom bar: the primary destinations from `navItems`, then a
	More tab that opens the sheet holding the rest.
-->
<script lang="ts">
	import { Ellipsis } from 'lucide-svelte';
	import NavItemBadge from './NavItemBadge.svelte';
	import { canSee, isMoreActive, navHref, navItems, type NavViewer } from './navItems';

	interface Props {
		currentPath: string;
		viewer: NavViewer;
		updateAvailable: boolean;
		onsearch: () => void;
		onmore: () => void;
	}

	let { currentPath, viewer, updateAvailable, onsearch, onmore }: Props = $props();

	const items = $derived(navItems.filter((item) => item.bottom && canSee(item, viewer)));
	// Five tabs and More when Settings is there, four and More when it is not.
	const compact = $derived(items.length < 5);
</script>

<nav
	class="droppedneedle-bottom-nav md:hidden"
	class:droppedneedle-bottom-nav--no-settings={compact}
	aria-label="Primary navigation"
>
	{#each items as item (item.id)}
		{@const active = item.bottom?.active(currentPath) ?? false}
		{#if item.href === undefined}
			<button
				type="button"
				class="droppedneedle-bottom-nav__item"
				class:active
				onclick={onsearch}
				aria-current={active ? 'page' : undefined}
			>
				<item.icon />
				<span>{item.label}</span>
			</button>
		{:else}
			<a
				href={navHref(item, updateAvailable)}
				class="droppedneedle-bottom-nav__item"
				class:active
				aria-current={active ? 'page' : undefined}
			>
				<item.icon />
				<span>{item.label}</span>
				{#if item.badge}
					<NavItemBadge badge={item.badge} place="bottom" {updateAvailable} />
				{/if}
			</a>
		{/if}
	{/each}
	<button
		type="button"
		class="droppedneedle-bottom-nav__item"
		class:active={isMoreActive(currentPath)}
		onclick={onmore}
		aria-label="More navigation options"
		aria-haspopup="dialog"
	>
		<Ellipsis />
		<span>More</span>
	</button>
</nav>

<style>
	.droppedneedle-bottom-nav {
		position: fixed;
		right: 0;
		bottom: 0;
		left: 0;
		z-index: 60;
		display: grid;
		grid-template-columns: repeat(6, minmax(0, 1fr));
		min-height: calc(var(--ms-bottom-nav-height) + var(--ms-safe-bottom));
		padding: 0.45rem 0.5rem calc(0.45rem + var(--ms-safe-bottom));
		border-top: 1px solid oklch(from var(--color-base-content) l c h / 0.08);
		background: oklch(from var(--color-base-200) l c h / 0.94);
		backdrop-filter: blur(18px) saturate(1.05);
		-webkit-backdrop-filter: blur(18px) saturate(1.05);
		box-shadow: 0 -10px 28px rgb(0 0 0 / 0.34);
	}

	/* Non-admins have no Settings tab, so the bar holds five slots and keeps them evenly spaced. */
	.droppedneedle-bottom-nav--no-settings {
		grid-template-columns: repeat(5, minmax(0, 1fr));
	}

	.droppedneedle-bottom-nav__item {
		position: relative;
		display: inline-flex;
		min-width: 0;
		min-height: 3.5rem;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.2rem;
		border-radius: 0.75rem;
		color: oklch(from var(--color-base-content) l c h / 0.62);
		font-size: 0.6875rem;
		font-weight: 700;
		line-height: 1.05;
		text-align: center;
	}

	.droppedneedle-bottom-nav__item :global(svg) {
		width: 1.35rem;
		height: 1.35rem;
	}

	.droppedneedle-bottom-nav__item.active {
		color: oklch(from var(--color-accent) l c h);
		background: oklch(from var(--color-accent) l c h / 0.11);
	}

	@media (max-width: 420px) {
		.droppedneedle-bottom-nav {
			padding-inline: 0.35rem;
		}

		.droppedneedle-bottom-nav__item {
			font-size: 0.625rem;
			min-height: 3.25rem;
		}
	}

	@media (min-width: 768px) {
		/* the base `.droppedneedle-bottom-nav` rule is unlayered, so it outranks Tailwind's utilities layer and `md:hidden` can't hide it; hide it here instead */
		.droppedneedle-bottom-nav {
			display: none;
		}
	}
</style>
