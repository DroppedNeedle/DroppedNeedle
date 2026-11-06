<script lang="ts" module>
	let activeMenu: HTMLDetailsElement | null = null;

	export function closeAllMenus() {
		if (activeMenu) {
			activeMenu.open = false;
			activeMenu = null;
		}
	}

	export interface MenuPoint {
		x: number;
		y: number;
	}

	interface MenuTriggerParams {
		open: (at: MenuPoint) => void;
		close: () => void;
		/** False leaves the element's own right-click and press alone. */
		enabled?: boolean;
	}

	const LONG_PRESS_MS = 500;
	const LONG_PRESS_SLOP_PX = 10;

	/**
	 * Opens a point menu from an element: a right-click, the Menu key,
	 * Shift+F10, or a long press on a touch screen. A drag that starts
	 * cancels a pending long press and closes the menu, so a draggable
	 * element keeps its drag.
	 */
	export function menuTrigger(node: HTMLElement, params: MenuTriggerParams) {
		let { open, close, enabled = true } = params;
		let timer: ReturnType<typeof setTimeout> | null = null;
		let start: MenuPoint | null = null;
		let pressOpened = false;

		function setCallout() {
			if (enabled) node.style.setProperty('-webkit-touch-callout', 'none');
			else node.style.removeProperty('-webkit-touch-callout');
		}
		setCallout();

		function cancelTimer() {
			if (timer) clearTimeout(timer);
			timer = null;
		}

		function besideNode(): MenuPoint {
			const rect = node.getBoundingClientRect();
			return { x: rect.left, y: rect.bottom };
		}

		function onContextMenu(e: MouseEvent) {
			if (!enabled) return;
			e.preventDefault();
			if (pressOpened) return;
			cancelTimer();
			if (start) {
				pressOpened = true;
				open(start);
				return;
			}
			// a keyboard-made event has no pointer position
			const fromKeyboard = e.clientX === 0 && e.clientY === 0;
			open(fromKeyboard ? besideNode() : { x: e.clientX, y: e.clientY });
		}

		function onKeyDown(e: KeyboardEvent) {
			if (!enabled) return;
			if (e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey)) {
				e.preventDefault();
				open(besideNode());
			}
		}

		function onPointerDown(e: PointerEvent) {
			pressOpened = false;
			cancelTimer();
			start = null;
			if (!enabled || e.pointerType !== 'touch') return;
			const at = { x: e.clientX, y: e.clientY };
			start = at;
			timer = setTimeout(() => {
				timer = null;
				pressOpened = true;
				open(at);
			}, LONG_PRESS_MS);
		}

		function onPointerMove(e: PointerEvent) {
			if (!timer || !start) return;
			if (Math.hypot(e.clientX - start.x, e.clientY - start.y) > LONG_PRESS_SLOP_PX) cancelTimer();
		}

		// the click that follows the release still needs pressOpened
		function onPointerEnd() {
			cancelTimer();
			start = null;
			setTimeout(() => (pressOpened = false), 0);
		}

		function onDragStart() {
			cancelTimer();
			close();
		}

		// the release after a long press would follow a link or press a button
		function onClick(e: MouseEvent) {
			if (!pressOpened) return;
			e.preventDefault();
			e.stopPropagation();
		}

		node.addEventListener('contextmenu', onContextMenu);
		node.addEventListener('keydown', onKeyDown);
		node.addEventListener('pointerdown', onPointerDown);
		node.addEventListener('pointermove', onPointerMove);
		node.addEventListener('pointerup', onPointerEnd);
		node.addEventListener('pointercancel', onPointerEnd);
		node.addEventListener('dragstart', onDragStart);
		node.addEventListener('click', onClick, true);

		return {
			update(next: MenuTriggerParams) {
				({ open, close, enabled = true } = next);
				setCallout();
			},
			destroy() {
				cancelTimer();
				node.removeEventListener('contextmenu', onContextMenu);
				node.removeEventListener('keydown', onKeyDown);
				node.removeEventListener('pointerdown', onPointerDown);
				node.removeEventListener('pointermove', onPointerMove);
				node.removeEventListener('pointerup', onPointerEnd);
				node.removeEventListener('pointercancel', onPointerEnd);
				node.removeEventListener('dragstart', onDragStart);
				node.removeEventListener('click', onClick, true);
			}
		};
	}
</script>

<script lang="ts">
	import { Ellipsis, EllipsisVertical } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { untrack, type Component, type SvelteComponent } from 'svelte';

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
		/** What the button is called; name the row when a list has many. */
		label?: string;
		/**
		 * `button` sits in a row of outlined buttons, at their height. `point`
		 * has no button: the menu is open while `at` is set, at that point
		 * (open it with `menuTrigger`).
		 */
		trigger?: 'icon' | 'button' | 'point';
		/** Where a point menu opens; null is closed. */
		at?: MenuPoint | null;
		/** A point menu asks to close: a pick, Escape, a wheel or swipe, or a press outside it. */
		onclose?: () => void;
	}

	let {
		items,
		position = 'end',
		size = 'sm',
		label = 'More actions',
		trigger = 'icon',
		at = null,
		onclose
	}: Props = $props();
	let detailsEl = $state<HTMLDetailsElement | null>(null);
	let summaryEl = $state<HTMLElement | null>(null);
	let menuEl = $state<HTMLUListElement | null>(null);
	let isOpen = $state(false);
	let menuTop = $state(0);
	let menuLeft = $state(0);
	const pointMode = $derived(trigger === 'point');
	let returnFocus: HTMLElement | null = null;

	function portal(node: HTMLElement) {
		document.body.appendChild(node);
		return {
			destroy() {
				node.remove();
			}
		};
	}

	function computeMenuPosition() {
		const point = pointMode && at ? { top: at.y, bottom: at.y, left: at.x, right: at.x } : null;
		const rect = point ?? summaryEl?.getBoundingClientRect();
		if (!rect) return;
		const gap = point ? 0 : 4;

		menuTop = rect.bottom + gap;
		menuLeft = point || position === 'start' ? rect.left : rect.right - MENU_WIDTH;

		if (menuLeft < VIEWPORT_MARGIN) menuLeft = VIEWPORT_MARGIN;
		if (menuLeft + MENU_WIDTH > window.innerWidth - VIEWPORT_MARGIN) {
			menuLeft = window.innerWidth - MENU_WIDTH - VIEWPORT_MARGIN;
		}

		const estimatedHeight = items.length * ITEM_HEIGHT_EST + MENU_PADDING;
		if (menuTop + estimatedHeight > window.innerHeight - VIEWPORT_MARGIN) {
			menuTop = rect.top - estimatedHeight - gap;
			if (menuTop < VIEWPORT_MARGIN) menuTop = VIEWPORT_MARGIN;
		}
	}

	function handleItemClick(item: MenuItem) {
		if (item.disabled) return;
		if (detailsEl) detailsEl.open = false;
		if (pointMode) onclose?.();
		item.onclick();
	}

	function handleEscape() {
		if (detailsEl) detailsEl.open = false;
		if (!pointMode) return;
		onclose?.();
		returnFocus?.focus({ preventScroll: true });
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
		if (isOpen && !pointMode) {
			document.addEventListener('click', handleClickOutside);
			window.addEventListener('scroll', handleScroll, true);
			return () => {
				document.removeEventListener('click', handleClickOutside);
				window.removeEventListener('scroll', handleScroll, true);
			};
		}
	});

	// A point has nothing to move with, so scrolling the page closes a point
	// menu. It listens for what a person does to scroll, not for scroll
	// events, which also come from boxes that scroll by themselves.
	$effect(() => {
		if (!pointMode || !at) return;
		closeAllMenus();
		// the position reads what it writes; only a new point moves it
		untrack(() => {
			computeMenuPosition();
			isOpen = true;
		});
		const close = () => onclose?.();
		const outside = (e: Event) => !menuEl?.contains(e.target as Node);
		const pressOutside = (e: PointerEvent) => {
			if (outside(e)) close();
		};
		const wheel = (e: WheelEvent) => {
			if (outside(e)) close();
		};
		// the finger of the long press that opened it may still be down
		let freshTouch = false;
		const touchStart = () => (freshTouch = true);
		const touchMove = (e: TouchEvent) => {
			if (freshTouch && outside(e)) close();
		};
		const escape = (e: KeyboardEvent) => {
			if (e.key === 'Escape') handleEscape();
		};
		const passive = { capture: true, passive: true };
		document.addEventListener('pointerdown', pressOutside, true);
		document.addEventListener('keydown', escape);
		window.addEventListener('wheel', wheel, passive);
		window.addEventListener('touchstart', touchStart, passive);
		window.addEventListener('touchmove', touchMove, passive);
		window.addEventListener('resize', close);
		return () => {
			isOpen = false;
			document.removeEventListener('pointerdown', pressOutside, true);
			document.removeEventListener('keydown', escape);
			window.removeEventListener('wheel', wheel, passive);
			window.removeEventListener('touchstart', touchStart, passive);
			window.removeEventListener('touchmove', touchMove, passive);
			window.removeEventListener('resize', close);
		};
	});

	// A point menu takes focus, so the keyboard can pick and Escape can hand it back.
	function focusFirst(node: HTMLElement) {
		if (!pointMode) return;
		const active = document.activeElement;
		returnFocus = active instanceof HTMLElement && !node.contains(active) ? active : null;
		node.querySelector<HTMLElement>('button:not([disabled])')?.focus({ preventScroll: true });
	}
</script>

{#if !pointMode}
	<details
		bind:this={detailsEl}
		class="dropdown {position === 'start' ? 'dropdown-start' : 'dropdown-end'}"
		ontoggle={handleToggle}
	>
		<IconButton
			as="summary"
			bind:ref={summaryEl}
			variant={trigger === 'button' ? 'outline' : 'ghost'}
			shape={trigger === 'button' ? 'square' : 'circle'}
			size={trigger === 'button' ? 'md' : size === 'xs' ? 'xs' : 'sm'}
			icon={trigger === 'button' ? Ellipsis : EllipsisVertical}
			{label}
			aria-haspopup="menu"
			onclick={(e: MouseEvent) => e.stopPropagation()}
		/>
	</details>
{/if}

{#if isOpen}
	<ul
		bind:this={menuEl}
		use:portal
		use:focusFirst
		class="menu w-52 rounded-box border border-line bg-base-200 p-2 shadow-lg"
		style="position: fixed; top: {menuTop}px; left: {menuLeft}px; z-index: 9999;"
		role="menu"
		aria-label={pointMode ? label : undefined}
		onclick={(e: MouseEvent) => e.stopPropagation()}
		oncontextmenu={(e: MouseEvent) => e.preventDefault()}
		onkeydown={(e: KeyboardEvent) => {
			if (e.key !== 'Escape') return;
			// keeps the point menu's document listener from running it twice
			if (pointMode) e.stopPropagation();
			handleEscape();
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
