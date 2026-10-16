<!--
	The desktop sidebar: the destinations from `navItems`, the connected
	services, and the icon row at the bottom (settings, log out, collapse).
-->
<script lang="ts">
	import { LogOut, PanelLeft } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SidebarServices from '$lib/components/SidebarServices.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
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
	{@const active = item.sidebar?.active?.(currentPath) ?? false}
	<li>
		{#if item.href === undefined}
			<button
				type="button"
				onclick={onsearch}
				class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
				data-tip={item.label}
				aria-label={item.label}
			>
				<item.icon class="h-6 w-6" />
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
		class="is-drawer-close:w-16 is-drawer-open:w-64 bg-base-200 flex flex-col items-start min-h-full max-h-dvh overflow-y-auto overflow-x-hidden"
	>
		<ul class="menu w-full grow p-2 [&_li>*]:py-3">
			{#each searchItems as item (item.id)}
				{@render entry(item)}
			{/each}

			<div class="divider my-0"></div>

			{#each browseItems as item (item.id)}
				{@render entry(item)}
			{/each}

			<SidebarServices />

			{#if requestItems.length > 0}
				<div class="divider my-0"></div>
				{#each requestItems as item (item.id)}
					{@render entry(item)}
				{/each}
			{/if}

			{#if adminItems.length > 0}
				<div class="divider my-0"></div>
				<li class="menu-title is-drawer-close:hidden px-3 pb-1 pt-2">
					<span class="font-mono text-[0.65rem] uppercase tracking-[0.18em] text-base-content/40"
						>Admin</span
					>
				</li>
				{#each adminItems as item (item.id)}
					{@render entry(item)}
				{/each}
			{/if}
		</ul>
		<div class="w-full p-2 flex flex-col gap-1" class:pb-24={playerStore.isPlayerVisible}>
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
