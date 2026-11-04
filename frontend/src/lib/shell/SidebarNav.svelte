<!--
	The desktop sidebar: the destinations from `navItems`, the connected
	services, and the icon row at the bottom (settings, log out, collapse).
	Closed it is a tight icon rail with tooltips; the current destination
	sits on the action fill.
-->
<script lang="ts">
	import { page } from '$app/state';
	import { LogOut, PanelLeft } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SidebarServices from '$lib/shell/SidebarServices.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { logout } from '$lib/utils/logout';
	import NavIcon from './NavIcon.svelte';
	import NavItemBadge from './NavItemBadge.svelte';
	import {
		canSee,
		navHref,
		navItems,
		type NavItem,
		type NavViewer,
		type SidebarGroup
	} from './navItems';

	interface Props {
		currentPath: string;
		viewer: NavViewer;
		updateAvailable: boolean;
		onsearch: () => void;
	}

	let { currentPath, viewer, updateAvailable, onsearch }: Props = $props();

	function inGroup(group: SidebarGroup): NavItem[] {
		return navItems.filter((item) => item.sidebar?.group === group && canSee(item, viewer));
	}

	const searchItems = $derived(inGroup('search'));
	const browseItems = $derived(inGroup('browse'));
	const requestItems = $derived(inGroup('requests'));
	const adminItems = $derived(inGroup('admin'));
	const footerItems = $derived(inGroup('footer'));

	function footerLabel(item: NavItem): string {
		return item.badge === 'update' && updateAvailable
			? `${item.label} - update available`
			: item.label;
	}
</script>

{#snippet entry(item: NavItem)}
	{@const active = item.sidebar?.active?.(currentPath, page.url.searchParams.get('tab')) ?? false}
	<li>
		{#if item.href === undefined}
			<button
				type="button"
				onclick={onsearch}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				data-tip={item.label}
				aria-label={item.label}
			>
				<item.icon class="h-5 w-5 shrink-0" />
				<span class="is-drawer-close:hidden">{item.label}</span>
			</button>
		{:else}
			<a
				href={navHref(item, updateAvailable)}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				class:menu-active={active}
				aria-current={active ? 'page' : undefined}
				aria-label={item.label}
				data-tip={item.label}
			>
				<NavIcon {item}>
					{#if item.badge}
						<NavItemBadge badge={item.badge} place="sidebar" {updateAvailable} />
					{/if}
				</NavIcon>
				<span class="is-drawer-close:hidden">{item.label}</span>
			</a>
		{/if}
	</li>
{/snippet}

<div class="drawer-side hidden md:block is-drawer-close:overflow-visible">
	<label for="main-drawer" aria-label="close sidebar" class="drawer-overlay"></label>
	<div
		class="dn-rail is-drawer-close:w-14 is-drawer-open:w-60 flex min-h-full max-h-dvh flex-col items-start overflow-x-hidden overflow-y-auto border-r border-base-300 bg-base-100"
	>
		<ul class="menu w-full grow gap-0.5 px-2 py-2">
			{#each searchItems as item (item.id)}
				{@render entry(item)}
			{/each}

			<div class="dn-rail__rule" aria-hidden="true"></div>

			{#each browseItems as item (item.id)}
				{@render entry(item)}
			{/each}

			<SidebarServices />

			{#if requestItems.length > 0}
				<div class="dn-rail__rule" aria-hidden="true"></div>
				{#each requestItems as item (item.id)}
					{@render entry(item)}
				{/each}
			{/if}

			{#if adminItems.length > 0}
				<div class="dn-rail__rule" aria-hidden="true"></div>
				<li class="menu-title is-drawer-close:hidden px-3 pb-1 pt-2">
					<!-- A trusted user sees only Check albums here. -->
					<span class="font-mono text-xs uppercase tracking-[0.16em] text-muted"
						>{viewer.isAdmin ? 'Admin' : 'Library'}</span
					>
				</li>
				{#each adminItems as item (item.id)}
					{@render entry(item)}
				{/each}
			{/if}
		</ul>
		<div
			class="flex w-full flex-col gap-1 border-t border-base-300 p-2"
			class:pb-24={playerStore.isPlayerVisible}
		>
			<!-- IconButton's own tooltip names these, so they skip the collapsed-sidebar tooltip. -->
			{#each footerItems as item (item.id)}
				<span class="relative inline-flex self-start">
					<IconButton
						href={navHref(item, updateAvailable)}
						icon={item.icon}
						label={footerLabel(item)}
					/>
					{#if item.badge}
						<NavItemBadge badge={item.badge} place="sidebar" {updateAvailable} />
					{/if}
				</span>
			{/each}
			<IconButton icon={LogOut} label="Log out" onclick={() => void logout()} />
			<IconButton
				as="label"
				for="main-drawer"
				icon={PanelLeft}
				label="Toggle sidebar"
				class="drawer-button is-drawer-open:rotate-y-180"
			/>
		</div>
	</div>
</div>

<style>
	/* rail items: one 40px target each, the icon centred while closed */
	.dn-rail :global(.menu li > :is(a, button)) {
		min-height: 2.5rem;
		padding: 0.625rem;
		border-radius: 0.5rem;
		color: var(--color-base-content);
		font-weight: 600;
		transition:
			background-color 150ms,
			color 150ms;
	}
	@media (hover: hover) {
		.dn-rail :global(.menu li > :is(a, button):not(.menu-active):hover) {
			background-color: var(--color-neutral);
		}
	}
	.dn-rail :global(.menu li > a.menu-active) {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
	.dn-rail :global(.dn-rail__rule) {
		height: 1px;
		margin: 0.375rem 0.5rem;
		background-color: var(--color-base-300);
	}
</style>
