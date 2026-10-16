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
