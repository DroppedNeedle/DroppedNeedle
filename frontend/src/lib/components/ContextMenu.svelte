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

	function handleScroll() {
		if (detailsEl?.open) {
			detailsEl.open = false;
		}
	}

	function handleToggle() {
		const nowOpen = detailsEl?.open ?? false;
		if (nowOpen) {
			if (activeMenu && activeMenu !== detailsEl) {
				activeMenu.open = false;
			}
			activeMenu = detailsEl;
			computeMenuPosition();
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
	<summary
		bind:this={summaryEl}
		class="btn btn-ghost btn-circle {size === 'xs' ? 'btn-xs' : 'btn-sm'}"
		aria-haspopup="menu"
		aria-label="More actions"
		onclick={(e: MouseEvent) => e.stopPropagation()}
	>
		<EllipsisVertical class={size === 'xs' ? 'h-3.5 w-3.5' : 'h-4 w-4'} />
	</summary>
</details>

{#if isOpen}
	<ul
		bind:this={menuEl}
		use:portal
		class="dn-ctx-menu menu w-52 rounded-xl p-2"
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
					class="rounded-lg text-[0.85rem] hover:bg-base-content/8 {item.disabled
						? 'opacity-50 cursor-not-allowed'
						: ''} {item.className ?? ''}"
					disabled={item.disabled}
					aria-disabled={item.disabled ? 'true' : undefined}
					onclick={(e: MouseEvent) => {
						e.stopPropagation();
						handleItemClick(item);
					}}
				>
					<item.icon class="h-4 w-4 opacity-70" />
					{item.label}
				</button>
			</li>
		{/each}
	</ul>
{/if}

<style>
	.dn-ctx-menu {
		border: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-200) l c h / 0.97);
		backdrop-filter: blur(24px) saturate(1.05);
		-webkit-backdrop-filter: blur(24px) saturate(1.05);
		box-shadow: var(--dn-shadow-4);
	}
</style>
