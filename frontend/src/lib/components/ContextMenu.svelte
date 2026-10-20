<script lang="ts" module>
	let activeMenu: HTMLDetailsElement | null = null;

	export function closeAllMenus() {
		if (activeMenu) {
			activeMenu.open = false;
			activeMenu = null;
		}
	}
</script>

<script lang="ts">
	import { EllipsisVertical } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import type { Component, SvelteComponent } from 'svelte';

	const MENU_WIDTH = 208;
	const ITEM_HEIGHT_EST = 40;
	const MENU_PADDING = 16;
	const VIEWPORT_MARGIN = 8;

	export interface MenuItem {
		label: string;
		// eslint-disable-next-line @typescript-eslint/no-explicit-any -- Svelte framework limitation: icon components have varying prop signatures
		icon: Component<Record<string, any>> | (new (...args: any[]) => SvelteComponent<any>);
		onclick: () => void;
		disabled?: boolean;
		className?: string;
	}

	interface Props {
		items: MenuItem[];
		position?: 'start' | 'end';
		size?: 'xs' | 'sm';
	}

	let { items, position = 'end', size = 'sm' }: Props = $props();
	let detailsEl = $state<HTMLDetailsElement | null>(null);
	let summaryEl = $state<HTMLElement | null>(null);
	let menuEl = $state<HTMLUListElement | null>(null);
	let isOpen = $state(false);
	let menuTop = $state(0);
	let menuLeft = $state(0);

	function portal(node: HTMLElement) {
		document.body.appendChild(node);
		return {
			destroy() {
				node.remove();
			}
		};
	}

	function computeMenuPosition() {
		if (!summaryEl) return;
		const rect = summaryEl.getBoundingClientRect();

		menuTop = rect.bottom + 4;
		menuLeft = position === 'end' ? rect.right - MENU_WIDTH : rect.left;

		if (menuLeft < VIEWPORT_MARGIN) menuLeft = VIEWPORT_MARGIN;
		if (menuLeft + MENU_WIDTH > window.innerWidth - VIEWPORT_MARGIN) {
			menuLeft = window.innerWidth - MENU_WIDTH - VIEWPORT_MARGIN;
		}

		const estimatedHeight = items.length * ITEM_HEIGHT_EST + MENU_PADDING;
		if (menuTop + estimatedHeight > window.innerHeight - VIEWPORT_MARGIN) {
			menuTop = rect.top - estimatedHeight - 4;
			if (menuTop < VIEWPORT_MARGIN) menuTop = VIEWPORT_MARGIN;
		}
	}

	function handleItemClick(item: MenuItem) {
		if (item.disabled) return;
		if (detailsEl) detailsEl.open = false;
		item.onclick();
	}

	function handleClickOutside(e: MouseEvent) {
		const target = e.target as Node;
		if (detailsEl?.open && !detailsEl.contains(target) && !menuEl?.contains(target)) {
			detailsEl.open = false;
		}
	}

	// Where the button sat when the menu opened, so a scroll that leaves it in
	// place (another box scrolling, or the page holding its place while images
	// above it load) keeps the menu open.
	let openedAt: { top: number; left: number } | null = null;

	function handleScroll() {
		if (!detailsEl?.open || !summaryEl) return;
		const rect = summaryEl.getBoundingClientRect();
		const moved =
			!openedAt || Math.abs(rect.top - openedAt.top) > 2 || Math.abs(rect.left - openedAt.left) > 2;
		if (moved) detailsEl.open = false;
	}

	function handleToggle() {
		const nowOpen = detailsEl?.open ?? false;
		if (nowOpen) {
			if (activeMenu && activeMenu !== detailsEl) {
				activeMenu.open = false;
			}
			activeMenu = detailsEl;
			computeMenuPosition();
			const rect = summaryEl?.getBoundingClientRect();
			openedAt = rect ? { top: rect.top, left: rect.left } : null;
		} else if (activeMenu === detailsEl) {
			activeMenu = null;
		}
		isOpen = nowOpen;
	}

	$effect(() => {
		if (isOpen) {
			document.addEventListener('click', handleClickOutside);
			window.addEventListener('scroll', handleScroll, true);
			return () => {
				document.removeEventListener('click', handleClickOutside);
				window.removeEventListener('scroll', handleScroll, true);
			};
		}
	});
</script>

<details
	bind:this={detailsEl}
	class="dropdown {position === 'start' ? 'dropdown-start' : 'dropdown-end'}"
	ontoggle={handleToggle}
>
	<IconButton
		as="summary"
		bind:ref={summaryEl}
		size={size === 'xs' ? 'xs' : 'sm'}
		icon={EllipsisVertical}
		label="More actions"
		aria-haspopup="menu"
		onclick={(e: MouseEvent) => e.stopPropagation()}
	/>
</details>

{#if isOpen}
	<ul
		bind:this={menuEl}
		use:portal
		class="menu bg-base-200/95 backdrop-blur-md rounded-box shadow-lg w-52 p-2"
		style="position: fixed; top: {menuTop}px; left: {menuLeft}px; z-index: 9999;"
		role="menu"
		onclick={(e: MouseEvent) => e.stopPropagation()}
		onkeydown={(e: KeyboardEvent) => {
			if (e.key === 'Escape' && detailsEl) detailsEl.open = false;
		}}
	>
		{#each items as item, i (`item-${i}`)}
			<li>
				<button
					role="menuitem"
					class="{item.disabled ? 'opacity-50 cursor-not-allowed' : ''} {item.className ?? ''}"
					disabled={item.disabled}
					aria-disabled={item.disabled ? 'true' : undefined}
					onclick={(e: MouseEvent) => {
						e.stopPropagation();
						handleItemClick(item);
					}}
				>
					<item.icon class="h-4 w-4" />
					{item.label}
				</button>
			</li>
		{/each}
	</ul>
{/if}
